import type { JWK } from "jose";
import { isRecord, requireInteger, requireString } from "./canonical";
import {
  deleteTunnelAndDns,
  getTunnel,
  provisionTunnel,
  refreshTunnelRuntime,
  type ProvisionedTunnel,
} from "./cloudflare";
import {
  assertManagedHostname,
  authenticateRelayRequest,
  authenticateWorkosRequest,
  deriveAllocatedHostname,
  issueRelayToken,
  relaySigningJwk,
  signMintProof,
  verifyEnvironmentSignature,
} from "./crypto";
import {
  acquireProvisionLease,
  acquireTunnelDeleteLease,
  clearTunnelAllocation,
  completeProvision,
  consumeLinkChallenge,
  createLinkChallenge,
  createOrReactivateEnvironment,
  failProvision,
  listEnvironments,
  listHostnameAliases,
  loadLinkChallenge,
  requireEnvironment,
  releaseTunnelDeleteLease,
  unlinkEnvironment,
  updateEnvironmentStatus,
} from "./db";
import {
  ApiError,
  errorResponse,
  json,
  readJson,
  readOptionalJson,
  readResponseJson,
} from "./errors";
import type {
  DpopIdentity,
  Env,
  EnvironmentRow,
  EnvironmentStatus,
  LinkBody,
  MintResponse,
} from "./types";

const ENVIRONMENT_ID = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;
const CLIENT_NONCE = /^[A-Za-z0-9_-]+$/;

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    try {
      return await route(request, env);
    } catch (error) {
      return errorResponse(error);
    }
  },
} satisfies ExportedHandler<Env>;

async function route(request: Request, env: Env): Promise<Response> {
  const url = new URL(request.url);
  if (url.pathname === "/v1/auth/token" && request.method === "POST") {
    const identity = await authenticateWorkosRequest(request, env);
    return json(await issueRelayToken(env, identity, identity.jkt));
  }

  if (url.pathname === "/v1/environments/link/challenge" && request.method === "POST") {
    const identity = await authenticateWorkosRequest(request, env);
    const body = requireObject(await readJson<unknown>(request));
    const environmentId = requireEnvironmentId(body.environment_id);
    const name = requireEnvironmentName(body.name);
    return json(await createLinkChallenge(env.DB, identity, environmentId, name), 201);
  }

  if (url.pathname === "/v1/environments/link" && request.method === "POST") {
    const identity = await authenticateWorkosRequest(request, env);
    return handleLink(request, env, identity);
  }

  if (url.pathname === "/v1/environments" && request.method === "GET") {
    const identity = await authenticateRelayRequest(request, env);
    const environments = await listEnvironments(env.DB, identity);
    return json({ environments: environments.map(environmentResource) });
  }

  const match = /^\/v1\/environments\/([^/]+?)(?:\/(status|connect|tunnel))?$/.exec(url.pathname);
  if (match) {
    const identity = await authenticateRelayRequest(request, env);
    const environmentId = requireEnvironmentId(decodePathSegment(match[1]));
    const action = match[2];
    if (action === "status" && request.method === "GET") {
      return handleStatus(env, identity, environmentId);
    }
    if (action === "connect" && request.method === "POST") {
      return handleConnect(request, env, identity, environmentId);
    }
    if (action === "tunnel" && request.method === "DELETE") {
      return handleDeleteTunnel(request, env, identity, environmentId);
    }
    if (!action && request.method === "DELETE") {
      const environment = await requireEnvironment(env.DB, environmentId, identity);
      await unlinkEnvironment(env.DB, environment);
      return new Response(null, {
        status: 204,
        headers: {
          "cache-control": "no-store",
          "x-environment-generation": String(environment.generation + 1),
        },
      });
    }
  }

  throw new ApiError(404, "not_found", "The relay route was not found");
}

async function handleLink(request: Request, env: Env, identity: DpopIdentity): Promise<Response> {
  const raw = requireObject(await readJson<unknown>(request));
  const publicJwk = requireObject(raw.environment_public_jwk) as JWK;
  const body: LinkBody = {
    challenge_id: requireString(raw.challenge_id, "challenge_id", { max: 128 }),
    environment_id: requireEnvironmentId(raw.environment_id),
    name: requireEnvironmentName(raw.name),
    environment_public_jwk: publicJwk,
    issued_at: requireTimestampValue(raw.issued_at, "issued_at"),
    signature: requireString(raw.signature, "signature", {
      min: 40,
      max: 512,
      pattern: /^[A-Za-z0-9_-]+$/,
    }),
  };
  const challenge = await loadLinkChallenge(env.DB, body.challenge_id, identity);
  if (challenge.environment_id !== body.environment_id || challenge.environment_name !== body.name) {
    throw new ApiError(400, "link_challenge_mismatch", "The link proof does not match its challenge");
  }
  const now = Math.floor(Date.now() / 1000);
  const issuedAtSeconds = timestampSeconds(body.issued_at, "issued_at");
  if (
    issuedAtSeconds < now - 300 ||
    issuedAtSeconds > now + 5 ||
    issuedAtSeconds < challenge.created_at - 5 ||
    issuedAtSeconds > challenge.expires_at
  ) {
    throw new ApiError(401, "stale_link_proof", "The link proof is outside the accepted time window");
  }
  const signedProof = {
    challenge: challenge.challenge,
    challenge_id: body.challenge_id,
    environment_id: body.environment_id,
    environment_public_jwk: body.environment_public_jwk,
    issued_at: body.issued_at,
    name: body.name,
  };
  const publicJkt = await verifyEnvironmentSignature(publicJwk, signedProof, body.signature);
  await consumeLinkChallenge(env.DB, body.challenge_id);

  const hostname = await deriveAllocatedHostname(
    env.RELAY_ISSUER,
    env.PUBLIC_HOSTNAME,
    identity.sub,
    body.environment_id,
  );
  let environment = await createOrReactivateEnvironment(env.DB, {
    identity,
    id: body.environment_id,
    name: body.name,
    publicJwk,
    publicJkt,
    hostname,
  });
  const allocation = await acquireProvisionLease(env.DB, environment);
  environment = allocation.environment;
  const aliases = (await listHostnameAliases(env.DB, environment.id)).map((alias) => alias.hostname);

  let runtime: ProvisionedTunnel;
  try {
    runtime = environment.tunnel_id
      ? await refreshTunnelRuntime(
          env,
          environment.tunnel_id,
          environment.hostname,
          aliases,
        )
      : await provisionTunnel(env, environment.hostname, aliases);
    await completeProvision(env.DB, {
      environmentId: environment.id,
      lease: allocation.lease,
      tunnelId: runtime.tunnelId,
      dnsRecordId: runtime.dnsRecordId,
      status: runtime.status,
    });
  } catch (error) {
    await failProvision(env.DB, environment.id, allocation.lease);
    throw error;
  }

  environment = await requireEnvironment(env.DB, environment.id, identity);
  const endpoint = environmentEndpoint(environment, env);
  return json(
    {
      environment: environmentResource(environment),
      endpoint,
      runtime: {
        tunnel_id: runtime.tunnelId,
        hostname: environment.hostname,
        connector_token: runtime.connectorToken,
        ingress: runtime.ingress,
        relay_signing_public_jwk: relaySigningJwk(env).publicJwk,
      },
    },
    200,
  );
}

async function handleStatus(env: Env, identity: DpopIdentity, environmentId: string): Promise<Response> {
  const environment = await requireEnvironment(env.DB, environmentId, identity);
  if (!environment.tunnel_id) return json({ environment: environmentResource(environment) });
  const tunnel = await getTunnel(env, environment.tunnel_id);
  const status = cloudflareTunnelStatus(tunnel.status);
  await updateEnvironmentStatus(env.DB, environment.id, status);
  const current = { ...environment, status, updated_at: Math.floor(Date.now() / 1000) };
  return json({ environment: environmentResource(current) });
}

async function handleConnect(
  request: Request,
  env: Env,
  identity: DpopIdentity,
  environmentId: string,
): Promise<Response> {
  const body = requireObject(await readJson<unknown>(request));
  const clientNonce = requireString(body.client_nonce, "client_nonce", {
    min: 16,
    max: 256,
    pattern: CLIENT_NONCE,
  });
  const environment = await requireEnvironment(env.DB, environmentId, identity);
  if (!environment.tunnel_id) {
    throw new ApiError(409, "environment_not_provisioned", "The environment has no provisioned tunnel");
  }
  const endpoint = environmentEndpoint(environment, env);
  const { proof } = await signMintProof(env, {
    ownerSub: identity.sub,
    orgId: identity.orgId,
    environmentId: environment.id,
    endpoint,
    clientJkt: identity.jkt,
    clientNonce,
    generation: environment.generation,
  });

  const mintUrl = new URL("/v1/connect/mint", `${endpoint.http_base_url}/`);
  const response = await fetch(mintUrl, {
    method: "POST",
    redirect: "error",
    signal: AbortSignal.timeout(15_000),
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ proof }),
  });
  if (!response.ok) {
    throw new ApiError(502, "environment_mint_failed", "The environment rejected the mint request");
  }
  const mint = await parseMintResponse(response);
  const now = Math.floor(Date.now() / 1000);
  const expiresAtSeconds = timestampSeconds(mint.expires_at, "expires_at");
  if (
    mint.environment_id !== environment.id ||
    mint.client_jkt !== identity.jkt ||
    mint.client_nonce !== clientNonce ||
    expiresAtSeconds <= now ||
    expiresAtSeconds > now + 300
  ) {
    throw new ApiError(502, "invalid_environment_mint", "The environment mint response does not match the request");
  }
  let publicJwk: JWK;
  try {
    publicJwk = JSON.parse(environment.public_jwk) as JWK;
  } catch {
    throw new ApiError(500, "invalid_pinned_key", "The pinned environment key is unreadable");
  }
  try {
    await verifyEnvironmentSignature(
      publicJwk,
      {
        environment_id: mint.environment_id,
        bootstrap_credential: mint.bootstrap_credential,
        expires_at: mint.expires_at,
        client_jkt: mint.client_jkt,
        client_nonce: mint.client_nonce,
      },
      mint.signature,
    );
  } catch {
    throw new ApiError(502, "invalid_environment_signature", "The environment mint signature is invalid");
  }
  return json({
    environment_id: environment.id,
    workspace_id: environment.id,
    endpoint,
    bootstrap_credential: mint.bootstrap_credential,
    expires_at: mint.expires_at,
    client_jkt: mint.client_jkt,
  });
}

async function handleDeleteTunnel(
  request: Request,
  env: Env,
  identity: DpopIdentity,
  environmentId: string,
): Promise<Response> {
  const environment = await requireEnvironment(env.DB, environmentId, identity, { includeUnlinked: true });
  const optionalBody = await readOptionalJson<unknown>(request);
  let expectedGeneration = environment.generation;
  if (optionalBody !== undefined) {
    const body = requireObject(optionalBody);
    expectedGeneration = requireInteger(body.generation, "generation");
  }
  if (expectedGeneration !== environment.generation) {
    throw new ApiError(409, "generation_changed", "The environment changed; refresh before deleting its tunnel");
  }
  if (!environment.tunnel_id && environment.status === "unlinked") {
    return new Response(null, { status: 204, headers: { "cache-control": "no-store" } });
  }
  const deleteToken = await acquireTunnelDeleteLease(env.DB, environment, expectedGeneration);
  try {
    if (environment.tunnel_id) {
      const aliases = await listHostnameAliases(env.DB, environment.id);
      await deleteTunnelAndDns(env, environment.tunnel_id, [
        environment.dns_record_id,
        ...aliases.map((alias) => alias.dns_record_id),
      ]);
    }
    await clearTunnelAllocation(env.DB, environment, expectedGeneration, deleteToken);
  } catch (error) {
    await releaseTunnelDeleteLease(env.DB, environment.id, deleteToken);
    throw error;
  }
  return new Response(null, { status: 204, headers: { "cache-control": "no-store" } });
}

function environmentResource(environment: EnvironmentRow): Record<string, unknown> {
  return {
    id: environment.id,
    environment_id: environment.id,
    workspace_id: environment.id,
    name: environment.name,
    status: environment.status,
    endpoint: endpointForHostname(environment.hostname),
    hostname: environment.hostname,
    tunnel_id: environment.tunnel_id,
    generation: environment.generation,
    created_at: environment.created_at,
    updated_at: environment.updated_at,
  };
}

function environmentEndpoint(
  environment: EnvironmentRow,
  env: Env,
): { http_base_url: string; ws_base_url: string } {
  return endpointForHostname(assertManagedHostname(environment.hostname, env.PUBLIC_HOSTNAME));
}

function endpointForHostname(hostname: string): { http_base_url: string; ws_base_url: string } {
  return {
    http_base_url: `https://${hostname}`,
    ws_base_url: `wss://${hostname}`,
  };
}

function cloudflareTunnelStatus(status: string | undefined): EnvironmentStatus {
  return status === "healthy" || status === "degraded" || status === "down" || status === "inactive"
    ? status
    : "inactive";
}

function requireObject(value: unknown): Record<string, unknown> {
  if (!isRecord(value)) throw new ApiError(400, "invalid_request", "The request body must be a JSON object");
  return value;
}

function requireEnvironmentId(value: unknown): string {
  return requireString(value, "environment_id", { max: 128, pattern: ENVIRONMENT_ID });
}

function requireEnvironmentName(value: unknown): string {
  const name = requireString(value, "name", { max: 120 });
  if (name !== name.trim() || /[\u0000-\u001f\u007f]/.test(name)) {
    throw new ApiError(400, "invalid_request", "name is not valid");
  }
  return name;
}

function requireTimestampValue(value: unknown, field: string): string | number {
  if (typeof value !== "string" && typeof value !== "number") {
    throw new ApiError(400, "invalid_request", `${field} must be an RFC 3339 string or Unix timestamp`);
  }
  timestampSeconds(value, field);
  return value;
}

function timestampSeconds(value: string | number, field: string): number {
  if (typeof value === "number") {
    if (!Number.isSafeInteger(value) || value < 0) {
      throw new ApiError(400, "invalid_request", `${field} is not a valid timestamp`);
    }
    return value > 10_000_000_000 ? Math.floor(value / 1000) : value;
  }
  if (
    value.length > 64 ||
    !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/.test(value)
  ) {
    throw new ApiError(400, "invalid_request", `${field} is not a valid RFC 3339 timestamp`);
  }
  const milliseconds = Date.parse(value);
  if (!Number.isFinite(milliseconds)) {
    throw new ApiError(400, "invalid_request", `${field} is not a valid RFC 3339 timestamp`);
  }
  return Math.floor(milliseconds / 1000);
}

async function parseMintResponse(response: Response): Promise<MintResponse> {
  try {
    const rawMint = requireObject(await readResponseJson<unknown>(response));
    return {
      environment_id: requireString(rawMint.environment_id, "environment_id", { max: 128 }),
      bootstrap_credential: requireString(rawMint.bootstrap_credential, "bootstrap_credential", {
        min: 16,
        max: 16_384,
      }),
      expires_at: requireTimestampValue(rawMint.expires_at, "expires_at"),
      client_jkt: requireString(rawMint.client_jkt, "client_jkt", { min: 20, max: 128 }),
      client_nonce: requireString(rawMint.client_nonce, "client_nonce", { min: 16, max: 256 }),
      signature: requireString(rawMint.signature, "signature", {
        min: 40,
        max: 512,
        pattern: /^[A-Za-z0-9_-]+$/,
      }),
    };
  } catch {
    throw new ApiError(502, "invalid_environment_response", "The environment returned an invalid mint response");
  }
}

function decodePathSegment(value: string | undefined): string {
  if (!value) throw new ApiError(404, "not_found", "The environment was not found");
  try {
    return decodeURIComponent(value);
  } catch {
    throw new ApiError(400, "invalid_request", "The environment identifier is malformed");
  }
}
