import type { JWK } from "jose";
import { canonicalJson } from "./canonical";
import { ApiError } from "./errors";
import type { ChallengeRow, EnvironmentRow, EnvironmentStatus, Identity } from "./types";

export async function createLinkChallenge(
  db: D1Database,
  identity: Identity,
  environmentId: string,
  name: string,
): Promise<{ challenge_id: string; challenge: string; expires_at: string }> {
  const now = Math.floor(Date.now() / 1000);
  const id = crypto.randomUUID();
  const challenge = randomChallenge();
  const expiresAt = now + 300;
  await db.prepare("DELETE FROM link_challenges WHERE expires_at < ?1").bind(now).run();
  await db
    .prepare(
      `INSERT INTO link_challenges(
        id, challenge, environment_id, environment_name, owner_sub, org_id, expires_at, created_at
      ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)`,
    )
    .bind(id, challenge, environmentId, name, identity.sub, identity.orgId, expiresAt, now)
    .run();
  return { challenge_id: id, challenge, expires_at: new Date(expiresAt * 1000).toISOString() };
}

export async function loadLinkChallenge(
  db: D1Database,
  challengeId: string,
  identity: Identity,
): Promise<ChallengeRow> {
  const row = await db
    .prepare("SELECT * FROM link_challenges WHERE id = ?1")
    .bind(challengeId)
    .first<ChallengeRow>();
  const now = Math.floor(Date.now() / 1000);
  if (
    !row ||
    row.owner_sub !== identity.sub ||
    row.org_id !== identity.orgId ||
    row.used_at !== null ||
    row.expires_at < now
  ) {
    throw new ApiError(401, "invalid_link_challenge", "The link challenge is invalid, expired, or already used");
  }
  return row;
}

export async function consumeLinkChallenge(db: D1Database, challengeId: string): Promise<void> {
  const now = Math.floor(Date.now() / 1000);
  const result = await db
    .prepare(
      "UPDATE link_challenges SET used_at = ?2 WHERE id = ?1 AND used_at IS NULL AND expires_at >= ?2",
    )
    .bind(challengeId, now)
    .run();
  if ((result.meta.changes ?? 0) !== 1) {
    throw new ApiError(409, "link_challenge_replayed", "The link challenge has already been consumed");
  }
}

export async function createOrReactivateEnvironment(
  db: D1Database,
  input: {
    identity: Identity;
    id: string;
    name: string;
    publicJwk: JWK;
    publicJkt: string;
    hostname: string;
  },
): Promise<EnvironmentRow> {
  const existing = await db
    .prepare("SELECT * FROM environments WHERE id = ?1")
    .bind(input.id)
    .first<EnvironmentRow>();
  const now = Math.floor(Date.now() / 1000);
  if (existing) {
    if (existing.owner_sub !== input.identity.sub || existing.org_id !== input.identity.orgId) {
      throw new ApiError(409, "environment_owned", "That environment identifier is already linked");
    }
    if (existing.public_jkt !== input.publicJkt) {
      throw new ApiError(409, "environment_key_changed", "The environment signing key does not match the pinned key");
    }
    if (existing.delete_token !== null) {
      throw new ApiError(409, "environment_deleting", "That environment tunnel is being deleted; retry shortly");
    }
    if (existing.status === "unlinked") {
      const reactivated = await db
        .prepare(
          `UPDATE environments
           SET name = ?2, status = CASE WHEN tunnel_id IS NULL THEN 'provisioning' ELSE 'inactive' END,
               generation = generation + 1, unlinked_at = NULL, updated_at = ?3
           WHERE id = ?1 AND delete_token IS NULL AND provision_token IS NULL`,
        )
        .bind(input.id, input.name, now)
        .run();
      if ((reactivated.meta.changes ?? 0) !== 1) {
        throw new ApiError(409, "environment_busy", "That environment allocation is being changed; retry shortly");
      }
    } else if (existing.name !== input.name) {
      const renamed = await db
        .prepare(
          `UPDATE environments SET name = ?2, updated_at = ?3
           WHERE id = ?1 AND delete_token IS NULL AND provision_token IS NULL`,
        )
        .bind(input.id, input.name, now)
        .run();
      if ((renamed.meta.changes ?? 0) !== 1) {
        throw new ApiError(409, "environment_busy", "That environment allocation is being changed; retry shortly");
      }
    }
    return requireEnvironment(db, input.id, input.identity);
  }

  await db
    .prepare(
      `INSERT OR IGNORE INTO environments(
        id, name, owner_sub, org_id, public_jwk, public_jkt, hostname,
        generation, status, created_at, updated_at
      ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, 'provisioning', ?8, ?8)`,
    )
    .bind(
      input.id,
      input.name,
      input.identity.sub,
      input.identity.orgId,
      canonicalJson(input.publicJwk),
      input.publicJkt,
      input.hostname,
      now,
    )
    .run();
  const inserted = await db
    .prepare("SELECT * FROM environments WHERE id = ?1")
    .bind(input.id)
    .first<EnvironmentRow>();
  if (!inserted) {
    throw new ApiError(409, "allocation_collision", "The environment allocation collided; retry with a new identifier");
  }
  if (inserted.owner_sub !== input.identity.sub || inserted.org_id !== input.identity.orgId) {
    throw new ApiError(409, "environment_owned", "That environment identifier is already linked");
  }
  if (inserted.public_jkt !== input.publicJkt) {
    throw new ApiError(409, "environment_key_changed", "The environment signing key does not match the pinned key");
  }
  return inserted;
}

export async function acquireProvisionLease(
  db: D1Database,
  environment: EnvironmentRow,
): Promise<{ environment: EnvironmentRow; lease: string }> {
  const now = Math.floor(Date.now() / 1000);
  const lease = crypto.randomUUID();
  const result = await db
    .prepare(
      `UPDATE environments
       SET provision_token = ?2, provision_started_at = ?3, status = 'provisioning', updated_at = ?3
       WHERE id = ?1 AND delete_token IS NULL
         AND (provision_token IS NULL OR provision_started_at < ?4)`,
    )
    .bind(environment.id, lease, now, now - 120)
    .run();
  if ((result.meta.changes ?? 0) !== 1) {
    throw new ApiError(409, "environment_provisioning", "Environment provisioning or deletion is already in progress; retry shortly");
  }
  return {
    environment: await requireEnvironment(db, environment.id, {
      sub: environment.owner_sub,
      orgId: environment.org_id,
    }),
    lease,
  };
}

export async function completeProvision(
  db: D1Database,
  input: {
    environmentId: string;
    lease: string;
    tunnelId: string;
    dnsRecordId: string;
    status: EnvironmentStatus;
  },
): Promise<void> {
  const now = Math.floor(Date.now() / 1000);
  const result = await db
    .prepare(
      `UPDATE environments
       SET tunnel_id = ?3, dns_record_id = ?4, status = ?5,
           provision_token = NULL, provision_started_at = NULL, updated_at = ?6
       WHERE id = ?1 AND provision_token = ?2`,
    )
    .bind(
      input.environmentId,
      input.lease,
      input.tunnelId,
      input.dnsRecordId,
      input.status,
      now,
    )
    .run();
  if ((result.meta.changes ?? 0) !== 1) {
    throw new ApiError(409, "provision_generation_changed", "The environment allocation changed while provisioning");
  }
}

export async function failProvision(db: D1Database, environmentId: string, lease: string): Promise<void> {
  const now = Math.floor(Date.now() / 1000);
  await db
    .prepare(
      `UPDATE environments
       SET status = 'error', provision_token = NULL, provision_started_at = NULL, updated_at = ?3
       WHERE id = ?1 AND provision_token = ?2`,
    )
    .bind(environmentId, lease, now)
    .run();
}

export async function listEnvironments(db: D1Database, identity: Identity): Promise<EnvironmentRow[]> {
  const result = await db
    .prepare(
      `SELECT * FROM environments
       WHERE owner_sub = ?1 AND org_id = ?2 AND status != 'unlinked'
       ORDER BY created_at DESC`,
    )
    .bind(identity.sub, identity.orgId)
    .all<EnvironmentRow>();
  return result.results;
}

export async function requireEnvironment(
  db: D1Database,
  environmentId: string,
  identity: Identity,
  options: { includeUnlinked?: boolean } = {},
): Promise<EnvironmentRow> {
  const row = await db
    .prepare("SELECT * FROM environments WHERE id = ?1 AND owner_sub = ?2 AND org_id = ?3")
    .bind(environmentId, identity.sub, identity.orgId)
    .first<EnvironmentRow>();
  if (!row || (!options.includeUnlinked && row.status === "unlinked")) {
    throw new ApiError(404, "environment_not_found", "The environment was not found");
  }
  return row;
}

export async function updateEnvironmentStatus(
  db: D1Database,
  environmentId: string,
  status: EnvironmentStatus,
): Promise<void> {
  await db
    .prepare("UPDATE environments SET status = ?2, updated_at = ?3 WHERE id = ?1")
    .bind(environmentId, status, Math.floor(Date.now() / 1000))
    .run();
}

export async function unlinkEnvironment(
  db: D1Database,
  environment: EnvironmentRow,
): Promise<void> {
  const now = Math.floor(Date.now() / 1000);
  const result = await db
    .prepare(
      `UPDATE environments
       SET status = 'unlinked', generation = generation + 1, unlinked_at = ?3, updated_at = ?3
       WHERE id = ?1 AND generation = ?2 AND provision_token IS NULL AND delete_token IS NULL`,
    )
    .bind(environment.id, environment.generation, now)
    .run();
  if ((result.meta.changes ?? 0) !== 1) {
    throw new ApiError(409, "generation_changed", "The environment changed; refresh before unlinking");
  }
}

export async function clearTunnelAllocation(
  db: D1Database,
  environment: EnvironmentRow,
  expectedGeneration: number,
  deleteToken: string,
): Promise<void> {
  const now = Math.floor(Date.now() / 1000);
  const result = await db
    .prepare(
      `UPDATE environments
       SET tunnel_id = NULL, dns_record_id = NULL, status = 'unlinked',
           generation = generation + 1, delete_token = NULL, delete_started_at = NULL,
           updated_at = ?4
       WHERE id = ?1 AND generation = ?2 AND delete_token = ?3`,
    )
    .bind(environment.id, expectedGeneration, deleteToken, now)
    .run();
  if ((result.meta.changes ?? 0) !== 1) {
    throw new ApiError(409, "generation_changed", "The environment changed during tunnel deletion");
  }
}

export async function acquireTunnelDeleteLease(
  db: D1Database,
  environment: EnvironmentRow,
  expectedGeneration: number,
): Promise<string> {
  const now = Math.floor(Date.now() / 1000);
  const token = crypto.randomUUID();
  const result = await db
    .prepare(
      `UPDATE environments
       SET delete_token = ?3, delete_started_at = ?4, updated_at = ?4
       WHERE id = ?1 AND generation = ?2
         AND (delete_token IS NULL OR delete_started_at < ?5)
         AND (provision_token IS NULL OR provision_started_at < ?5)`,
    )
    .bind(environment.id, expectedGeneration, token, now, now - 120)
    .run();
  if ((result.meta.changes ?? 0) !== 1) {
    throw new ApiError(409, "environment_deleting", "That environment tunnel is already being deleted");
  }
  return token;
}

export async function releaseTunnelDeleteLease(
  db: D1Database,
  environmentId: string,
  deleteToken: string,
): Promise<void> {
  await db
    .prepare(
      `UPDATE environments
       SET delete_token = NULL, delete_started_at = NULL, updated_at = ?3
       WHERE id = ?1 AND delete_token = ?2`,
    )
    .bind(environmentId, deleteToken, Math.floor(Date.now() / 1000))
    .run();
}

export async function listHostnameAliases(
  db: D1Database,
  environmentId: string,
): Promise<Array<{ hostname: string; dns_record_id: string | null }>> {
  const result = await db
    .prepare(
      "SELECT hostname, dns_record_id FROM environment_hostname_aliases WHERE environment_id = ?1 ORDER BY created_at",
    )
    .bind(environmentId)
    .all<{ hostname: string; dns_record_id: string | null }>();
  return result.results;
}

function randomChallenge(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(32));
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/=/g, "").replace(/\+/g, "-").replace(/\//g, "_");
}
