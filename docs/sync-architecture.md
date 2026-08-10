# Clippy centralized sync

Clippy sync has two hosted pieces and no always-on Clippy server:

- Convex is the authenticated coordination and operation database.
- A private Cloudflare R2 bucket holds encrypted attachment chunks.

The former Cloudflare Tunnel, loopback origin, relay Worker, D1 database, and
`cloudflared` process are retired. WorkOS remains the login provider. Clippy
clients keep the existing CRDT and end-to-end encryption, so Convex and
Cloudflare receive ciphertext and routing metadata rather than note or file
contents.

## Data flow

Every device owns a random actor UUID. Local mutations become immutable CRDT
operations with a monotonically increasing actor counter.

1. A client scans or commits local changes.
2. It groups up to 256 consecutive operations into a payload no larger than
   550 KB, encrypts it with ChaCha20-Poly1305, and writes one `operationBatches`
   document to Convex.
3. Each device row stores only its latest accepted counter. The iOS app
   subscribes to that small `sync:changes` query, so note payloads are not read
   merely to detect a change. Desktop sync wakes on local changes and uses a
   30-second visible or five-minute hidden safety check.
4. A pull reads at most 12 missing encrypted batches. Clients authenticate the
   workspace, actor, and counter range as AEAD associated data before applying
   the operations idempotently.

Convex has four tables: `workspaces`, `devices`, `operationBatches`, and
short-lived `enrollments`. It does not store file bytes, plaintext entity
projections, OAuth tokens, or workspace encryption keys. Batches are append
only in schema version 1; future compaction must preserve a snapshot before
deleting history.

## Attachments and R2

Files are split into one-MiB content-addressed chunks. The existing manifest
contains the full-file SHA-256, size, and ordered chunk hashes. Each chunk is
encrypted independently with workspace-and-hash-bound AAD.

Before publishing a manifest operation, a client asks the Convex
`storage:prepareUploads` action about at most 64 hashes. The action performs R2
HEAD requests and returns five-minute PUT URLs only for missing objects. The
client uploads ciphertext directly to R2. Downloads use the equivalent
five-minute GET URLs. Convex never proxies the bytes.

Objects use this private key layout:

```text
v1/<sha256 of WorkOS token identifier>/<workspace UUID>/<chunk sha256>.e2ee
```

Keep the bucket private. Native clients use R2's S3 API endpoint through signed
URLs, so browser CORS is not required. Cloudflare R2 presigned URLs do not work
through a custom domain. `clippyr2.saudecomalex.com` can later be a separate
login-protected human portal, but the sync transport must continue using the
private S3 endpoint unless a Cloudflare Worker is added. No Worker is required
for the implementation in this repository.

## Authentication and enrollment

Each Convex deployment trusts the matching WorkOS AuthKit issuer and client ID.
Every query, mutation, and action checks the authenticated WorkOS token
identifier. One WorkOS account owns one workspace per deployment; staging and
production remain isolated.

The first signed-in Mac creates the workspace UUID and a random 256-bit
workspace key. The key remains in macOS Keychain. A signed-in iPhone without a
key publishes a ten-minute enrollment request containing an ephemeral X25519
public key. The Mac publishes a one-use X25519/HKDF-wrapped grant, and the phone
stores the recovered workspace key in its ThisDeviceOnly Keychain before
accepting device membership. Convex sees the public keys and encrypted grant,
not the workspace key.

## Login persistence

OAuth access, ID, and refresh tokens never use Keychain. Desktop stores one
environment-scoped session record in Clippy's private SQLite database. Mobile
stores the equivalent record in its app sandbox using atomic writes, iOS file
protection after first unlock, mode `0600`, and backup exclusion. Temporary
network, WorkOS, or signing-key failures retain that local session and retry;
only an explicit sign-out, a rejected refresh, or an invalid token removes it.
Both apps expose a per-device sign-out action. It stops that device's active
sync transport and clears its OAuth session without deleting local content or
the workspace key.

Existing builds used Keychain for OAuth credentials. The new clients do not
read those legacy entries, avoiding an access prompt; upgrading therefore
requires one sign-in to create the new app-private session.

Workspace encryption keys are not OAuth tokens. They remain in device-only
Keychain storage so extracting a local database or token file is not sufficient
to decrypt synced content.

Device revocation and workspace-key rotation are still required before treating
sync as production-ready. A compromised unlocked enrolled device can read its
local plaintext; centralized sync cannot protect against endpoint compromise.

## Deployment setup

The `clippy` Convex project uses separate staging and production deployments,
backed by the private `clippy-staging` and `clippy-production` R2 buckets.

1. Copy `.env.example` to `.env.local` and run `npm run convex:dev` to connect a
   development deployment. Convex will generate its normal `_generated` files.
2. In each Convex deployment set `WORKOS_ISSUER`, `WORKOS_CLIENT_ID`,
   `R2_ACCOUNT_ID`, `R2_BUCKET`, `R2_ACCESS_KEY_ID`, and
   `R2_SECRET_ACCESS_KEY` with `npx convex env set`. Use an R2 token scoped only
   to the selected bucket.
3. Desktop and iOS builds carry only the public staging and production Convex
   URLs. `CLIPPY_CONVEX_URL` remains a local desktop override.
4. Keep R2 credentials only in the matching Convex deployment environment.
5. Run `npm run convex:deploy` for the selected deployment. Verify WorkOS JWT
   issuer/audience and R2 bucket isolation separately in staging and production.

Useful local validation:

```sh
npm run convex:check
npm run build
cd ios && swift test
```

The checked-in `convex/_generated/server.ts` is a small pre-account typecheck
shim. `convex dev` replaces it with deployment-generated types.
