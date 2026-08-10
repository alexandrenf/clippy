import type { JWK } from "jose";

export interface Env {
  DB: D1Database;
  WORKOS_ISSUER: string;
  WORKOS_AUDIENCE: string;
  RELAY_ISSUER: string;
  PUBLIC_HOSTNAME: string;
  ORIGIN_PORT: string;
  CLOUDFLARE_ACCOUNT_ID: string;
  CLOUDFLARE_ZONE_ID: string;
  CLOUDFLARE_API_TOKEN: string;
  RELAY_TOKEN_SECRET: string;
  RELAY_SIGNING_PRIVATE_JWK: string;
}

export interface Identity {
  sub: string;
  orgId: string;
}

export interface DpopIdentity extends Identity {
  jkt: string;
}

export interface EnvironmentRow {
  id: string;
  name: string;
  owner_sub: string;
  org_id: string;
  public_jwk: string;
  public_jkt: string;
  hostname: string;
  tunnel_id: string | null;
  dns_record_id: string | null;
  generation: number;
  status: EnvironmentStatus;
  provision_token: string | null;
  provision_started_at: number | null;
  delete_token: string | null;
  delete_started_at: number | null;
  created_at: number;
  updated_at: number;
  unlinked_at: number | null;
}

export type EnvironmentStatus =
  | "provisioning"
  | "inactive"
  | "healthy"
  | "degraded"
  | "down"
  | "error"
  | "unlinked";

export interface ChallengeRow {
  id: string;
  challenge: string;
  environment_id: string;
  environment_name: string;
  owner_sub: string;
  org_id: string;
  expires_at: number;
  used_at: number | null;
  created_at: number;
}

export interface LinkBody {
  challenge_id: string;
  environment_id: string;
  name: string;
  environment_public_jwk: JWK;
  issued_at: string | number;
  signature: string;
}

export interface MintResponse {
  environment_id: string;
  bootstrap_credential: string;
  expires_at: string | number;
  client_jkt: string;
  client_nonce: string;
  signature: string;
}

export interface CloudflareEnvelope<T> {
  success: boolean;
  result: T;
  errors?: Array<{ code?: number; message?: string }>;
}

export interface CloudflareTunnel {
  id: string;
  name?: string;
  status?: "inactive" | "degraded" | "healthy" | "down";
  config_src?: "local" | "cloudflare";
  deleted_at?: string | null;
}

export interface CloudflareDnsRecord {
  id: string;
  name: string;
  type: string;
  content: string;
  proxied?: boolean;
}
