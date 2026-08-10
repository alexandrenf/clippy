PRAGMA foreign_keys = ON;

CREATE TABLE dpop_jtis (
  jti TEXT PRIMARY KEY,
  jkt TEXT NOT NULL,
  htm TEXT NOT NULL,
  htu TEXT NOT NULL,
  expires_at INTEGER NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE INDEX idx_dpop_jtis_expires_at ON dpop_jtis(expires_at);

CREATE TABLE link_challenges (
  id TEXT PRIMARY KEY,
  challenge TEXT NOT NULL UNIQUE,
  environment_id TEXT NOT NULL,
  environment_name TEXT NOT NULL,
  owner_sub TEXT NOT NULL,
  org_id TEXT NOT NULL,
  expires_at INTEGER NOT NULL,
  used_at INTEGER,
  created_at INTEGER NOT NULL
);

CREATE INDEX idx_link_challenges_owner
  ON link_challenges(owner_sub, org_id, environment_id, created_at DESC);
CREATE INDEX idx_link_challenges_expires_at ON link_challenges(expires_at);

CREATE TABLE environments (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  owner_sub TEXT NOT NULL,
  org_id TEXT NOT NULL,
  public_jwk TEXT NOT NULL,
  public_jkt TEXT NOT NULL,
  hostname TEXT NOT NULL UNIQUE,
  tunnel_id TEXT UNIQUE,
  dns_record_id TEXT,
  generation INTEGER NOT NULL DEFAULT 1,
  status TEXT NOT NULL CHECK(status IN ('provisioning', 'inactive', 'healthy', 'degraded', 'down', 'error', 'unlinked')),
  provision_token TEXT,
  provision_started_at INTEGER,
  delete_token TEXT,
  delete_started_at INTEGER,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  unlinked_at INTEGER
);

CREATE INDEX idx_environments_owner
  ON environments(owner_sub, org_id, status, created_at DESC);

CREATE TABLE environment_hostname_aliases (
  environment_id TEXT NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
  hostname TEXT NOT NULL UNIQUE,
  dns_record_id TEXT,
  is_legacy INTEGER NOT NULL DEFAULT 1 CHECK(is_legacy IN (0, 1)),
  created_at INTEGER NOT NULL,
  PRIMARY KEY(environment_id, hostname)
);
