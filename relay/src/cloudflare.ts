import { assertManagedHostname, loopbackOrigin } from "./crypto";
import { isRecord } from "./canonical";
import { ApiError } from "./errors";
import type {
  CloudflareDnsRecord,
  CloudflareEnvelope,
  CloudflareTunnel,
  Env,
  EnvironmentStatus,
} from "./types";

const API_ROOT = "https://api.cloudflare.com/client/v4";

export interface ProvisionedTunnel {
  tunnelId: string;
  dnsRecordId: string;
  connectorToken: string;
  status: EnvironmentStatus;
  ingress: Array<{ hostname?: string; service: string }>;
}

export async function provisionTunnel(
  env: Env,
  hostname: string,
  aliases: string[],
): Promise<ProvisionedTunnel> {
  const managedHostname = assertManagedHostname(hostname, env.PUBLIC_HOSTNAME);
  const checkedAliases = aliases.map((alias) =>
    assertManagedHostname(alias, env.PUBLIC_HOSTNAME, { allowLegacy: true }),
  );
  const tunnelName = `clippy-${managedHostname.split(".")[0]}`;
  const tunnel = await findOrCreateTunnel(env, tunnelName);
  const origin = loopbackOrigin(env.ORIGIN_PORT);
  const ingress: Array<{ hostname?: string; service: string }> = [managedHostname, ...checkedAliases].map((entry) => ({
    hostname: entry,
    service: origin,
  }));
  ingress.push({ service: "http_status:404" });

  await cfRequest(env, `/accounts/${account(env)}/cfd_tunnel/${encodeURIComponent(tunnel.id)}/configurations`, {
    method: "PUT",
    body: JSON.stringify({ config: { ingress } }),
  });
  const dns = await ensureDnsRecord(env, managedHostname, `${tunnel.id}.cfargotunnel.com`);
  const token = await cfRequest<string>(
    env,
    `/accounts/${account(env)}/cfd_tunnel/${encodeURIComponent(tunnel.id)}/token`,
  );
  if (typeof token !== "string" || token.length < 16) {
    throw new ApiError(502, "invalid_connector_token", "Cloudflare returned an invalid connector token");
  }
  return {
    tunnelId: tunnel.id,
    dnsRecordId: dns.id,
    connectorToken: token,
    status: tunnelStatus(tunnel.status),
    ingress,
  };
}

export async function refreshTunnelRuntime(
  env: Env,
  tunnelId: string,
  hostname: string,
  aliases: string[],
): Promise<ProvisionedTunnel> {
  const managedHostname = assertManagedHostname(hostname, env.PUBLIC_HOSTNAME, { allowLegacy: true });
  const tunnel = await getTunnel(env, tunnelId);
  const origin = loopbackOrigin(env.ORIGIN_PORT);
  const ingress: Array<{ hostname?: string; service: string }> = [managedHostname, ...aliases.map((alias) =>
    assertManagedHostname(alias, env.PUBLIC_HOSTNAME, { allowLegacy: true }),
  )].map((entry) => ({ hostname: entry, service: origin }));
  ingress.push({ service: "http_status:404" });
  await cfRequest(env, `/accounts/${account(env)}/cfd_tunnel/${encodeURIComponent(tunnel.id)}/configurations`, {
    method: "PUT",
    body: JSON.stringify({ config: { ingress } }),
  });
  const dns = await ensureDnsRecord(env, managedHostname, `${tunnel.id}.cfargotunnel.com`);
  const connectorToken = await cfRequest<string>(
    env,
    `/accounts/${account(env)}/cfd_tunnel/${encodeURIComponent(tunnel.id)}/token`,
  );
  if (typeof connectorToken !== "string" || connectorToken.length < 16) {
    throw new ApiError(502, "invalid_connector_token", "Cloudflare returned an invalid connector token");
  }
  return {
    tunnelId: tunnel.id,
    dnsRecordId: dns.id,
    connectorToken,
    status: tunnelStatus(tunnel.status),
    ingress,
  };
}

export async function getTunnel(env: Env, tunnelId: string): Promise<CloudflareTunnel> {
  const tunnel = await cfRequest<CloudflareTunnel>(
    env,
    `/accounts/${account(env)}/cfd_tunnel/${encodeURIComponent(tunnelId)}`,
  );
  if (!tunnel.id) throw new ApiError(502, "invalid_tunnel", "Cloudflare returned an invalid tunnel");
  return tunnel;
}

export async function deleteTunnelAndDns(
  env: Env,
  tunnelId: string,
  dnsRecordIds: Array<string | null>,
): Promise<void> {
  await cfRequest(
    env,
    `/accounts/${account(env)}/cfd_tunnel/${encodeURIComponent(tunnelId)}`,
    { method: "DELETE" },
    { allowNotFound: true },
  );
  for (const recordId of new Set(dnsRecordIds.filter((id): id is string => Boolean(id)))) {
    await cfRequest(env, `/zones/${zone(env)}/dns_records/${encodeURIComponent(recordId)}`, {
      method: "DELETE",
    }, { allowNotFound: true });
  }
}

function account(env: Env): string {
  if (!/^[A-Za-z0-9_-]{3,64}$/.test(env.CLOUDFLARE_ACCOUNT_ID)) {
    throw new ApiError(500, "invalid_configuration", "CLOUDFLARE_ACCOUNT_ID is invalid");
  }
  return encodeURIComponent(env.CLOUDFLARE_ACCOUNT_ID);
}

function zone(env: Env): string {
  if (!/^[A-Za-z0-9_-]{3,64}$/.test(env.CLOUDFLARE_ZONE_ID)) {
    throw new ApiError(500, "invalid_configuration", "CLOUDFLARE_ZONE_ID is invalid");
  }
  return encodeURIComponent(env.CLOUDFLARE_ZONE_ID);
}

async function findOrCreateTunnel(env: Env, name: string): Promise<CloudflareTunnel> {
  const query = new URLSearchParams({ name, is_deleted: "false", per_page: "100" });
  const tunnels = await cfRequest<CloudflareTunnel[]>(
    env,
    `/accounts/${account(env)}/cfd_tunnel?${query.toString()}`,
  );
  if (!Array.isArray(tunnels)) {
    throw new ApiError(502, "invalid_tunnel_list", "Cloudflare returned an invalid tunnel list");
  }
  const existing = tunnels.find((tunnel) => tunnel.name === name && !tunnel.deleted_at);
  if (existing?.id) {
    if (existing.config_src && existing.config_src !== "cloudflare") {
      throw new ApiError(409, "tunnel_source_conflict", "The existing tunnel is not remotely managed");
    }
    return existing;
  }
  const created = await cfRequest<CloudflareTunnel>(env, `/accounts/${account(env)}/cfd_tunnel`, {
    method: "POST",
    body: JSON.stringify({ name, config_src: "cloudflare" }),
  });
  if (!created.id) throw new ApiError(502, "invalid_tunnel", "Cloudflare returned an invalid tunnel");
  return created;
}

async function ensureDnsRecord(
  env: Env,
  hostname: string,
  target: string,
): Promise<CloudflareDnsRecord> {
  const query = new URLSearchParams({ name: hostname, per_page: "100" });
  const records = await cfRequest<CloudflareDnsRecord[]>(
    env,
    `/zones/${zone(env)}/dns_records?${query.toString()}`,
  );
  const existing = records.find((record) => record.name.toLowerCase() === hostname);
  if (existing) {
    if (existing.type !== "CNAME" || existing.content.toLowerCase() !== target.toLowerCase()) {
      throw new ApiError(409, "hostname_conflict", "The managed hostname already has a conflicting DNS record");
    }
    if (existing.proxied) return existing;
    return cfRequest<CloudflareDnsRecord>(
      env,
      `/zones/${zone(env)}/dns_records/${encodeURIComponent(existing.id)}`,
      {
        method: "PATCH",
        body: JSON.stringify({ type: "CNAME", name: hostname, content: target, proxied: true, ttl: 1 }),
      },
    );
  }
  return cfRequest<CloudflareDnsRecord>(env, `/zones/${zone(env)}/dns_records`, {
    method: "POST",
    body: JSON.stringify({ type: "CNAME", name: hostname, content: target, proxied: true, ttl: 1 }),
  });
}

async function cfRequest<T = unknown>(
  env: Env,
  path: string,
  init: RequestInit = {},
  options: { allowNotFound?: boolean } = {},
): Promise<T> {
  let response: Response;
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort("cloudflare-control-plane-timeout"), 15_000);
  try {
    response = await fetch(`${API_ROOT}${path}`, {
      ...init,
      redirect: "manual",
      signal: controller.signal,
      headers: {
        authorization: `Bearer ${env.CLOUDFLARE_API_TOKEN}`,
        "content-type": "application/json",
        ...init.headers,
      },
    });
  } catch (error) {
    const detail = error instanceof Error
      ? error.message
          .slice(0, 240)
          .replace(/[A-Za-z0-9_-]{24,}/g, "[redacted-id]")
      : "non-error failure";
    console.error("cloudflare_control_plane_fetch_failed", {
      kind: error instanceof Error ? error.name : typeof error,
      detail,
    });
    throw new ApiError(502, "cloudflare_api_unreachable", "The Cloudflare control plane could not be reached");
  } finally {
    clearTimeout(timeout);
  }
  if (response.status >= 300 && response.status < 400) {
    throw new ApiError(502, "cloudflare_api_redirect", "Cloudflare unexpectedly redirected the control-plane request");
  }
  if (options.allowNotFound && response.status === 404) return undefined as T;
  let parsed: unknown;
  try {
    parsed = await response.json();
  } catch {
    throw new ApiError(502, "cloudflare_api_error", "Cloudflare returned an unreadable response");
  }
  if (!isRecord(parsed) || typeof parsed.success !== "boolean" || !("result" in parsed)) {
    throw new ApiError(502, "cloudflare_api_error", "Cloudflare returned an invalid response envelope");
  }
  const envelope = parsed as unknown as CloudflareEnvelope<T>;
  if (!response.ok || envelope.success !== true) {
    const remoteCode = envelope.errors?.[0]?.code;
    throw new ApiError(
      502,
      "cloudflare_api_error",
      `Cloudflare rejected the control-plane request${remoteCode ? ` (${remoteCode})` : ""}`,
    );
  }
  return envelope.result;
}

function tunnelStatus(value: CloudflareTunnel["status"]): EnvironmentStatus {
  return value === "healthy" || value === "degraded" || value === "down" || value === "inactive"
    ? value
    : "inactive";
}
