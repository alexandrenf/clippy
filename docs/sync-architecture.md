# Clippy multi-device sync

Status: usable end-to-end implementation pending live-device acceptance. The
repository includes the authenticated loopback origin, database scanner and
projector, bounded delta pagination, end-to-end encrypted chunk transfer,
atomic attachment reconstruction, desktop pairing action, persistent paired
Cloudflare connector and relay, durable iPhone replica, foreground WebSocket
hints, and menu-bar state. APNs, camera QR scanning, device revocation/key
rotation, and live physical-device acceptance are not complete; do not treat
the current build as production-ready sync yet.

## Environments

Staging and Production are intentionally isolated. A build must never infer
Production values from Staging.

| Setting | Staging | Production |
| --- | --- | --- |
| Public hostname | `https://clippy-staging.saudecomalex.com` | `https://clippy.saudecomalex.com` |
| Loopback origin | `127.0.0.1:49832` | `127.0.0.1:49833` |
| Desktop OAuth callback | `http://127.0.0.1:49834/auth/callback` | same path, separately registered |
| iOS callback | `clippy-sync://auth/callback` | same URI, separately registered |
| iOS bundle ID | `app.clippy.mobile` | `app.clippy.mobile` |
| WorkOS audience | `client_01KZMNQXBXWT2A807NZCE6V2HV` | `client_01KZMNK73NWS9NDAPC3T54S2PE` |
| WorkOS issuer | `https://fashionable-machine-85-staging.authkit.app` | `https://brave-mermaid-84.authkit.app` |
| WorkOS policy | development providers | six-digit email Magic Auth only |

The client IDs, hostnames, and issuer URLs are public routing metadata. WorkOS
API keys, OAuth client secrets, Cloudflare tunnel tokens, access/refresh tokens,
and workspace encryption keys must never be placed in source, xcconfig, logs,
process arguments, or SQLite.

The installed desktop defaults to Production, generates and persists a
workspace UUID on first pairing, and remembers the chosen environment in
SQLite. `CLIPPY_SYNC_ENVIRONMENT`, `CLIPPY_SYNC_WORKSPACE_ID`,
`CLIPPY_SYNC_TUNNEL_URL`, `CLIPPY_WORKOS_ISSUER`, and
`CLIPPY_WORKOS_AUDIENCE` are development overrides, not requirements for a
normally launched app. iOS uses separate Staging and Production xcconfig files.

## Authentication

The normal desktop flow starts from the **Sign in** button in Clippy Settings.
Clippy opens the system browser and listens only on `127.0.0.1:49834`; AuthKit
redirects back automatically, so there is no authorization code to copy and no
terminal involved. The app checks a random OAuth `state`, validates the OIDC
`nonce`, verifies RS256 signatures from `<issuer>/oauth2/jwks`, enforces
issuer/audience/expiry/subject, compares the access-token and ID-token subjects,
stores tokens in environment-separated macOS Keychain entries, and links the
selected relay environment.

The `clippy-sync` CLI provides the same browser/loopback login as an optional
developer and recovery path:

```sh
cd src-tauri
cargo run --bin clippy-sync -- login --environment staging
```

Neither flow uses a public-client secret or prints token material.

iOS uses `ASWebAuthenticationSession` and PKCE. Tokens use environment-scoped,
ThisDeviceOnly Keychain accessibility. Both the iPhone and origin verify RS256
signatures and issuer/audience/expiry/subject against WorkOS JWKS; the iPhone
also validates the OIDC nonce before storage and refreshes or signs out when a
refresh token is invalid. The origin pins the WorkOS `sub` plus optional
`org_id` to the workspace. Token values and Authorization headers are never
logged.

## Data model and convergence

Every device has a random actor UUID. Every immutable operation has an actor and
monotonic counter (`Dot`); the delta frontier is the greatest counter observed
for each actor (`VersionVector`). Duplicate operations are harmless.

- Section, item metadata, attachment metadata, and tombstones use an LWW
  register ordered by `(counter, actor UUID)`.
- Item content uses a causal multi-value register. A later edit supersedes only
  versions it observed. Concurrent edits remain as separate values and set an
  explicit conflict. A resolution operation observes all variants and replaces
  them. A deterministic projection is available for legacy views, but variants
  remain persisted and are never silently discarded.
- A future schema version can migrate item content to a sequence CRDT for
  character-level collaboration. Mixed schema versions must reject writes they
  cannot interpret rather than downgrade them.
- Tombstones are permanent in schema v1. Entity IDs are never reused; a future
  explicit undelete/compaction protocol must be introduced before this changes.

The wire JSON in Rust and Swift uses the same camel-case schema. SQLite stores
immutable operations, frontiers, conflicts, and verified chunk locations in
additive `sync_*` tables, leaving current Clippy IDs and tables intact.

## Pairing and end-to-end encryption

Cloudflare terminates edge TLS, so TLS is necessary but not the only protection.
All deltas and file chunks are encrypted before reaching the tunnel.

1. The Mac generates one persistent random 256-bit workspace key and stores it
   in Keychain. Pairing another device never rotates or replaces this key.
2. For each pairing, the Mac creates an ephemeral X25519 key and 256-bit
   one-time token. The QR/out-of-band offer contains schema version, workspace,
   expected hostname, WorkOS issuer/audience, Mac public key, token, and expiry.
3. The authenticated iPhone returns its ephemeral public key and token over TLS.
   The Mac checks expiry, token in constant time, and the validated WorkOS
   subject/organization.
4. Both sides derive a one-use wrap key with X25519 and HKDF-SHA256. Pairing AAD
   length-prefixes and binds version, workspace, tunnel URL, WorkOS
   issuer/audience, expiry, both public keys, WorkOS `sub`, and optional
   `org_id`.
5. The Mac encrypts the existing workspace key in a pairing grant using
   ChaCha20-Poly1305 with a random 96-bit nonce. The iPhone unwraps and stores it
   in Keychain. The ephemeral private keys and one-time token are discarded.
6. Delta and chunk envelopes use ChaCha20-Poly1305 with a fresh random nonce and
   workspace/version AAD. Receivers reject replayed operation IDs and failed
   authentication before mutation.

WorkOS authenticates people; it never derives or receives the workspace key.
No Cloudflare Access service token is embedded in the iPhone app.

## File protocol

Files are split into 1 MiB chunks by default. A manifest records the full-file
SHA-256, byte length, chunk size, and ordered `(SHA-256, length)` list.

- `POST /v1/sync/chunks/missing` returns hashes absent at the origin.
- `PUT /v1/sync/chunks/{sha256}` is idempotent and carries an E2EE envelope.
- `GET /v1/sync/chunks/{sha256}` resumes individual missing chunks.
- The receiver decrypts and verifies every plaintext chunk hash, reconstructs
  into a temporary file, verifies total size and full-file hash, then atomically
  installs it. A path/name from another device is metadata, never a filesystem
  destination.

## Transport and scheduling

The named Cloudflare Tunnel maps the environment hostname to its loopback-only
origin. Cloudflare Tunnel supports HTTPS/WebSockets and uses outbound-only
connections. The Mac origin must still require a valid WorkOS JWT, E2EE, body
limits, and unauthenticated rate limiting.

`cloudflared` is not installed as an OS service. Once a workspace is paired it
stays alive as a child of the running Clippy process so a foreground phone can
wake a hidden Mac without polling. `TunnelRunner` reads the
environment-specific token from macOS Keychain service
`app.clippy.desktop.sync`, account `cloudflare-tunnel:staging` or
`cloudflare-tunnel:production`, writes a `0600` temporary token file, launches
`cloudflared tunnel --no-autoupdate run --token-file <path>` with
stdin/stdout/stderr detached, retains the owner-only file for the child lifetime,
then unlinks it on stop/drop. Initial activation is on pairing or restoration
of a prior workspace. Child restart uses capped exponential backoff with jitter.

The steady state is push-driven over WebSocket. Hints contain no application
state; they only trigger an authenticated delta exchange. Every desktop
mutation already converges through one refresh hook, which wakes the scanner
immediately. A 30-second visible / five-minute hidden scan is only a safety net
for out-of-process database changes. iOS opens one authenticated socket only in
the foreground, wakes immediately on connection/change hints and local writes,
uses capped reconnect backoff, and keeps a five-minute safety exchange. It
cancels the socket/timer in the background. APNs is deferred until credentials
are provisioned.

Menu-bar states are `idle`, `syncing`, `synced`, and `waitingForDevice`.
Template-safe dot/ring overlays change only the small tray glyph and tooltip.

## Threat model and required server checks

Protected: clipboard text, sections/items, attachments, workspace key, OAuth
tokens, tunnel credential, and operation history.

Assumed trusted: each unlocked paired device, its OS Keychain, WorkOS issuer,
and the shipped binaries. Cloudflare, public networks, DNS observers, and other
users are not trusted with plaintext. A locally compromised unlocked device can
read that device's clipboard and decrypted database; remote sync cannot solve
endpoint compromise.

Implemented origin controls:

1. Bind only the environment-specific loopback port; WorkOS audience and E2EE
   workspace AAD prevent staging/production cross-use.
2. Verify JWT signature, algorithm, `iss`, `aud`, `exp`, `sub`, and optional
   workspace `org_id` with cached/rotating JWKS.
3. Isolate invalid-token rate buckets by Cloudflare client IP, apply a separate
   generous authenticated-principal bucket for chunk bursts, and enforce a
   12 MiB HTTP body cap, 8 MiB plaintext envelope cap, 2,000-operation pages,
   1,024 missing-hash queries, one-MiB chunks, and 250 MiB files.
4. Authenticate pairing before consuming its one-time token; allow one use and
   a short expiry.
5. Validate schema, workspace, actor/device membership, operation count, counter
   monotonicity, and JSON field sizes before inserting idempotently.
6. Decrypt/authenticate before projecting operations; never log payloads,
   bearer headers, tokens, file names, or raw WorkOS responses.
Remaining production control: device revocation must rotate the workspace key
and re-pair retained devices. A Cloudflare origin rule/rate policy should also
be verified in both live environments before release.

References: [WorkOS standalone AuthKit token verification](https://workos.com/docs/authkit/connect/standalone),
[WorkOS public OAuth applications](https://workos.com/docs/authkit/connect/oauth/public-applications),
[Cloudflare Tunnel configuration](https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/configure-tunnels/),
and [Cloudflare Tunnel WebSocket support](https://developers.cloudflare.com/cloudflare-one/faq/cloudflare-tunnels-faq/).
