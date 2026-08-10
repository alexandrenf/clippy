import {
  SignJWT,
  calculateJwkThumbprint,
  createRemoteJWKSet,
  decodeProtectedHeader,
  importJWK,
  jwtVerify,
  type JWK,
  type JWTPayload,
} from "jose";
import { canonicalJson, isRecord, requireString } from "./canonical";
import { ApiError } from "./errors";
import type { DpopIdentity, Env, Identity } from "./types";

const encoder = new TextEncoder();
const WORKOS_JWKS = new Map<string, ReturnType<typeof createRemoteJWKSet>>();
const RELAY_AUDIENCE = "clippy-relay-control-plane";
const DPOP_MAX_AGE_SECONDS = 60;
const RELAY_TOKEN_TTL_SECONDS = 300;

export function base64url(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/=/g, "").replace(/\+/g, "-").replace(/\//g, "_");
}

export function decodeBase64url(value: string): Uint8Array<ArrayBuffer> {
  if (!/^[A-Za-z0-9_-]+$/.test(value)) {
    throw new ApiError(400, "invalid_base64url", "The signature must be base64url encoded");
  }
  const padded = value.replace(/-/g, "+").replace(/_/g, "/").padEnd(Math.ceil(value.length / 4) * 4, "=");
  try {
    return Uint8Array.from(atob(padded), (character) => character.charCodeAt(0));
  } catch {
    throw new ApiError(400, "invalid_base64url", "The signature must be base64url encoded");
  }
}

export async function sha256Base64url(value: string): Promise<string> {
  return base64url(new Uint8Array(await crypto.subtle.digest("SHA-256", encoder.encode(value))));
}

function authorizationToken(request: Request, scheme: "Bearer" | "DPoP"): string {
  const header = request.headers.get("authorization") ?? "";
  const prefix = `${scheme} `;
  if (!header.startsWith(prefix) || header.length <= prefix.length) {
    throw new ApiError(401, "invalid_authorization", `Authorization must use the ${scheme} scheme`);
  }
  return header.slice(prefix.length);
}

function workosJwks(env: Env): ReturnType<typeof createRemoteJWKSet> {
  let issuer: URL;
  try {
    issuer = new URL(env.WORKOS_ISSUER);
  } catch {
    throw new ApiError(500, "invalid_configuration", "WORKOS_ISSUER is not a valid URL");
  }
  if (issuer.protocol !== "https:") {
    throw new ApiError(500, "invalid_configuration", "WORKOS_ISSUER must use HTTPS");
  }
  const jwksUrl = new URL("/oauth2/jwks", issuer);
  const cacheKey = jwksUrl.toString();
  const cached = WORKOS_JWKS.get(cacheKey);
  if (cached) return cached;
  const remote = createRemoteJWKSet(jwksUrl, { cooldownDuration: 30_000, cacheMaxAge: 600_000 });
  WORKOS_JWKS.set(cacheKey, remote);
  return remote;
}

export async function verifyWorkosBearer(request: Request, env: Env): Promise<Identity & { token: string }> {
  const token = authorizationToken(request, "Bearer");
  let payload: JWTPayload;
  try {
    ({ payload } = await jwtVerify(token, workosJwks(env), {
      algorithms: ["RS256"],
      issuer: env.WORKOS_ISSUER,
      clockTolerance: 5,
    }));
  } catch {
    throw new ApiError(401, "invalid_workos_token", "The WorkOS access token is invalid or expired");
  }
  const sub = requireClaim(payload.sub, "sub");
  if (payload.client_id !== env.WORKOS_AUDIENCE) {
    throw new ApiError(401, "invalid_workos_token", "The WorkOS token belongs to another application");
  }
  const orgId = optionalClaim(payload.org_id, "org_id");
  if (typeof payload.exp !== "number") {
    throw new ApiError(401, "invalid_workos_token", "The WorkOS access token has no expiration");
  }
  return { sub, orgId, token };
}

export async function verifyRelayToken(request: Request, env: Env): Promise<Identity & { token: string; jkt: string }> {
  const token = authorizationToken(request, "DPoP");
  if (encoder.encode(env.RELAY_TOKEN_SECRET).byteLength < 32) {
    throw new ApiError(500, "invalid_configuration", "RELAY_TOKEN_SECRET must contain at least 32 bytes");
  }
  let payload: JWTPayload;
  try {
    ({ payload } = await jwtVerify(token, encoder.encode(env.RELAY_TOKEN_SECRET), {
      algorithms: ["HS256"],
      issuer: env.RELAY_ISSUER,
      audience: RELAY_AUDIENCE,
      clockTolerance: 5,
    }));
  } catch {
    throw new ApiError(401, "invalid_relay_token", "The relay token is invalid or expired");
  }
  const cnf = payload.cnf;
  if (!isRecord(cnf) || typeof cnf.jkt !== "string") {
    throw new ApiError(401, "invalid_relay_token", "The relay token is not bound to a DPoP key");
  }
  if (payload.scope !== "relay:environments") {
    throw new ApiError(401, "invalid_relay_scope", "The relay token does not authorize environment access");
  }
  return {
    sub: requireClaim(payload.sub, "sub"),
    orgId: optionalClaim(payload.org_id, "org_id"),
    jkt: cnf.jkt,
    token,
  };
}

export async function issueRelayToken(
  env: Env,
  identity: Identity,
  jkt: string,
): Promise<{ access_token: string; token_type: "DPoP"; expires_in: number; scope: string; cnf: { jkt: string } }> {
  if (encoder.encode(env.RELAY_TOKEN_SECRET).byteLength < 32) {
    throw new ApiError(500, "invalid_configuration", "RELAY_TOKEN_SECRET must contain at least 32 bytes");
  }
  const now = Math.floor(Date.now() / 1000);
  const scope = "relay:environments";
  const accessToken = await new SignJWT({
    ...(identity.orgId ? { org_id: identity.orgId } : {}),
    scope,
    cnf: { jkt },
  })
    .setProtectedHeader({ alg: "HS256", typ: "at+jwt" })
    .setIssuer(env.RELAY_ISSUER)
    .setAudience(RELAY_AUDIENCE)
    .setSubject(identity.sub)
    .setJti(crypto.randomUUID())
    .setIssuedAt(now)
    .setExpirationTime(now + RELAY_TOKEN_TTL_SECONDS)
    .sign(encoder.encode(env.RELAY_TOKEN_SECRET));
  return {
    access_token: accessToken,
    token_type: "DPoP",
    expires_in: RELAY_TOKEN_TTL_SECONDS,
    scope,
    cnf: { jkt },
  };
}

export async function verifyDpopProof(
  request: Request,
  db: D1Database,
  accessToken: string,
  expectedJkt?: string,
): Promise<{ jkt: string }> {
  const proof = request.headers.get("dpop");
  if (!proof) throw new ApiError(401, "missing_dpop", "A DPoP proof is required");

  let header: ReturnType<typeof decodeProtectedHeader>;
  try {
    header = decodeProtectedHeader(proof);
  } catch {
    throw new ApiError(401, "invalid_dpop", "The DPoP protected header is invalid");
  }
  const jwk = header.jwk as JWK | undefined;
  if (
    header.alg !== "ES256" ||
    header.typ?.toLowerCase() !== "dpop+jwt" ||
    !jwk ||
    jwk.kty !== "EC" ||
    jwk.crv !== "P-256" ||
    typeof jwk.x !== "string" ||
    typeof jwk.y !== "string" ||
    "d" in jwk
  ) {
    throw new ApiError(401, "invalid_dpop", "The DPoP proof must embed a public P-256 JWK and use ES256");
  }

  const key = await importJWK(jwk, "ES256");
  let payload: JWTPayload;
  try {
    ({ payload } = await jwtVerify(proof, key, { algorithms: ["ES256"] }));
  } catch {
    throw new ApiError(401, "invalid_dpop", "The DPoP proof signature is invalid");
  }

  const now = Math.floor(Date.now() / 1000);
  const htm = requireClaim(payload.htm, "htm").toUpperCase();
  const htu = requireClaim(payload.htu, "htu");
  const jti = requireClaim(payload.jti, "jti");
  if (htm !== request.method.toUpperCase() || htu !== normalizedHtu(request.url)) {
    throw new ApiError(401, "dpop_target_mismatch", "The DPoP proof targets a different request");
  }
  if (
    typeof payload.iat !== "number" ||
    payload.iat < now - DPOP_MAX_AGE_SECONDS ||
    payload.iat > now + 5
  ) {
    throw new ApiError(401, "stale_dpop", "The DPoP proof is outside the accepted time window");
  }
  if (jti.length < 8 || jti.length > 200) {
    throw new ApiError(401, "invalid_dpop", "The DPoP jti is invalid");
  }
  const expectedAth = await sha256Base64url(accessToken);
  if (payload.ath !== expectedAth) {
    throw new ApiError(401, "dpop_token_mismatch", "The DPoP proof is not bound to this access token");
  }
  const jkt = await calculateJwkThumbprint(jwk, "sha256");
  if (expectedJkt && jkt !== expectedJkt) {
    throw new ApiError(401, "dpop_key_mismatch", "The DPoP key does not match the relay token");
  }

  await db.prepare("DELETE FROM dpop_jtis WHERE expires_at < ?1").bind(now).run();
  const inserted = await db
    .prepare(
      "INSERT OR IGNORE INTO dpop_jtis(jti, jkt, htm, htu, expires_at, created_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
    )
    .bind(jti, jkt, htm, htu, now + DPOP_MAX_AGE_SECONDS + 5, now)
    .run();
  if ((inserted.meta.changes ?? 0) !== 1) {
    throw new ApiError(401, "dpop_replay", "This DPoP proof has already been used");
  }
  return { jkt };
}

export async function authenticateRelayRequest(request: Request, env: Env): Promise<DpopIdentity> {
  const relay = await verifyRelayToken(request, env);
  await verifyDpopProof(request, env.DB, relay.token, relay.jkt);
  return { sub: relay.sub, orgId: relay.orgId, jkt: relay.jkt };
}

export async function authenticateWorkosRequest(request: Request, env: Env): Promise<DpopIdentity> {
  const workos = await verifyWorkosBearer(request, env);
  const { jkt } = await verifyDpopProof(request, env.DB, workos.token);
  return { sub: workos.sub, orgId: workos.orgId, jkt };
}

export async function verifyEnvironmentSignature(
  publicJwk: JWK,
  signedValue: unknown,
  signature: string,
): Promise<string> {
  if (
    publicJwk.kty !== "OKP" ||
    publicJwk.crv !== "Ed25519" ||
    typeof publicJwk.x !== "string" ||
    publicJwk.d !== undefined
  ) {
    throw new ApiError(400, "invalid_environment_key", "The environment key must be a public Ed25519 JWK");
  }
  const key = await importJWK(publicJwk, "EdDSA");
  const valid = await crypto.subtle.verify(
    "Ed25519",
    key as CryptoKey,
    decodeBase64url(signature),
    encoder.encode(canonicalJson(signedValue)),
  );
  if (!valid) {
    throw new ApiError(401, "invalid_environment_signature", "The environment signature is invalid");
  }
  return calculateJwkThumbprint(publicJwk, "sha256");
}

export function relaySigningJwk(env: Env): { privateJwk: JWK; publicJwk: JWK } {
  let parsed: unknown;
  try {
    parsed = JSON.parse(env.RELAY_SIGNING_PRIVATE_JWK);
  } catch {
    throw new ApiError(500, "invalid_configuration", "RELAY_SIGNING_PRIVATE_JWK is invalid JSON");
  }
  if (
    !isRecord(parsed) ||
    parsed.kty !== "OKP" ||
    parsed.crv !== "Ed25519" ||
    typeof parsed.x !== "string" ||
    typeof parsed.d !== "string"
  ) {
    throw new ApiError(500, "invalid_configuration", "RELAY_SIGNING_PRIVATE_JWK must be an Ed25519 private JWK");
  }
  const privateJwk = parsed as JWK;
  const publicJwk: JWK = {
    kty: "OKP",
    crv: "Ed25519",
    x: parsed.x,
    alg: "EdDSA",
    use: "sig",
  };
  if (typeof parsed.kid === "string") publicJwk.kid = parsed.kid;
  return { privateJwk, publicJwk };
}

export async function signMintProof(
  env: Env,
  claims: {
    ownerSub: string;
    orgId: string;
    environmentId: string;
    endpoint: { http_base_url: string; ws_base_url: string };
    clientJkt: string;
    clientNonce: string;
    generation: number;
  },
): Promise<{ proof: string; publicJwk: JWK }> {
  const { privateJwk, publicJwk } = relaySigningJwk(env);
  const key = await importJWK(privateJwk, "EdDSA");
  const now = Math.floor(Date.now() / 1000);
  const kid = publicJwk.kid ?? (await calculateJwkThumbprint(publicJwk, "sha256"));
  const proof = await new SignJWT({
    ...(claims.orgId ? { org_id: claims.orgId } : {}),
    environment_id: claims.environmentId,
    endpoint: claims.endpoint,
    client_jkt: claims.clientJkt,
    client_nonce: claims.clientNonce,
    generation: claims.generation,
  })
    .setProtectedHeader({ alg: "EdDSA", typ: "JWT", kid })
    .setIssuer(env.RELAY_ISSUER)
    .setAudience(`clippy-env:${claims.environmentId}`)
    .setSubject(claims.ownerSub)
    .setJti(crypto.randomUUID())
    .setIssuedAt(now)
    .setExpirationTime(now + 60)
    .sign(key);
  return { proof, publicJwk };
}

export function normalizedHtu(rawUrl: string): string {
  const url = new URL(rawUrl);
  url.search = "";
  url.hash = "";
  return url.toString();
}

export async function deriveAllocatedHostname(
  _relayIssuer: string,
  publicHostname: string,
  _ownerSub: string,
  _environmentId: string,
): Promise<string> {
  // Each stage is one account-wide Clippy workspace. Keeping the environment
  // on the configured first-level hostname lets Cloudflare's managed edge
  // certificate cover it without a paid nested-wildcard certificate.
  return normalizePublicHostname(publicHostname);
}

export function normalizePublicHostname(hostname: string): string {
  const value = hostname.trim().toLowerCase().replace(/\.$/, "");
  if (
    value.length > 253 ||
    !/^(?=.{1,253}$)(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(value)
  ) {
    throw new ApiError(500, "invalid_configuration", "PUBLIC_HOSTNAME must be a DNS hostname");
  }
  return value;
}

export function assertManagedHostname(
  hostname: string,
  publicHostname: string,
  _options: { allowLegacy?: boolean } = {},
): string {
  const candidate = hostname.toLowerCase().replace(/\.$/, "");
  const root = normalizePublicHostname(publicHostname);
  const managed = candidate === root;
  if (!managed) {
    throw new ApiError(500, "unmanaged_hostname", "The environment hostname is outside the managed relay domain");
  }
  return candidate;
}

export function loopbackOrigin(portValue: string): string {
  const port = Number(portValue);
  if (!Number.isInteger(port) || port < 1 || port > 65_535) {
    throw new ApiError(500, "invalid_configuration", "ORIGIN_PORT must be between 1 and 65535");
  }
  return `http://127.0.0.1:${port}`;
}

function relayStage(relayIssuer: string): "staging" | "prod" {
  let issuer: URL;
  try {
    issuer = new URL(relayIssuer);
  } catch {
    throw new ApiError(500, "invalid_configuration", "RELAY_ISSUER is not a valid URL");
  }
  if (issuer.protocol !== "https:") {
    throw new ApiError(500, "invalid_configuration", "RELAY_ISSUER must use HTTPS");
  }
  return /(?:^|[.-])(?:staging|stage|dev)(?:[.-]|$)/.test(issuer.hostname) ? "staging" : "prod";
}

function requireClaim(value: unknown, name: string): string {
  try {
    return requireString(value, name, { min: 1, max: 512 });
  } catch {
    throw new ApiError(401, "invalid_token_claims", `The token has no valid ${name} claim`);
  }
}

function optionalClaim(value: unknown, name: string): string {
  if (value === undefined || value === null) return "";
  return requireClaim(value, name);
}
