# Clippy Connect relay contract

Status: protocol version 1. The relay is a control plane only. Sync payloads and
file chunks flow directly between the iPhone and the Mac environment endpoint.

## Identities and transports

- WorkOS authenticates the account to the relay.
- Every client owns a non-exportable P-256 signing key. Relay and environment
  access tokens are DPoP-bound to its RFC 7638 JWK thumbprint.
- Every Mac environment owns a persistent Ed25519 signing key in Keychain.
- The relay owns a separate Ed25519 signing key. A successful HTTPS link
  response returns its public JWK for the Mac to pin in Keychain.
- JSON field names are `snake_case`. Binary values and signatures are unpadded
  base64url. Timestamps are UTC RFC 3339 unless a JWT claim requires epoch
  seconds.

DPoP proofs use `typ: dpop+jwt`, `alg: ES256`, an embedded public `jwk`, and
claims `htm`, `htu`, `iat`, and unique `jti`. Proofs for an access token also
carry `ath = base64url(SHA-256(access_token))`. URLs are normalized absolute
HTTPS URLs without fragments; method matching is uppercase and exact.

## Relay API

WorkOS routes use `Authorization: Bearer <workos-access-token>` and a fresh
`DPoP` proof. Relay-token routes use `Authorization: DPoP <relay-token>` and a
fresh proof bound to that token.

### `POST /v1/auth/token`

Request body: `{}`.

Response:

```json
{
  "access_token": "opaque-signed-relay-token",
  "token_type": "DPoP",
  "expires_in": 300,
  "cnf": { "jkt": "client-key-thumbprint" }
}
```

### `POST /v1/environments/link/challenge`

Request:

```json
{ "environment_id": "uuid", "name": "Alexandre's Mac" }
```

Response:

```json
{
  "challenge_id": "uuid",
  "challenge": "random-base64url",
  "expires_at": "2026-08-09T20:00:00Z"
}
```

### `POST /v1/environments/link`

The Ed25519 signature covers canonical JSON containing exactly
`challenge`, `challenge_id`, `environment_id`, `environment_public_jwk`,
`issued_at`, and `name`.

```json
{
  "challenge_id": "uuid",
  "environment_id": "uuid",
  "name": "Alexandre's Mac",
  "environment_public_jwk": { "kty": "OKP", "crv": "Ed25519", "x": "..." },
  "issued_at": "2026-08-09T20:00:00Z",
  "signature": "..."
}
```

The relay consumes the challenge once, pins the WorkOS subject/organization
and environment key, idempotently provisions a remotely managed Cloudflare
Tunnel whose only ingress is the configured loopback origin plus a 404
catch-all, and creates a proxied CNAME at the stage's exact managed hostname.
The internal release has one account-wide environment per stage.

Response:

```json
{
  "environment": { "id": "uuid", "name": "Alexandre's Mac", "status": "linked" },
  "endpoint": {
    "http_base_url": "https://clippy.saudecomalex.com",
    "ws_base_url": "wss://clippy.saudecomalex.com"
  },
  "runtime": {
    "tunnel_id": "uuid",
    "hostname": "clippy.saudecomalex.com",
    "connector_token": "secret",
    "ingress": "http://127.0.0.1:49833",
    "relay_signing_public_jwk": { "kty": "OKP", "crv": "Ed25519", "x": "..." }
  }
}
```

The connector token is returned only to the linked Mac and is never exposed
by list or status APIs.

### Discovery and lifecycle

- `GET /v1/environments`
- `GET /v1/environments/:id/status`
- `DELETE /v1/environments/:id`
- `DELETE /v1/environments/:id/tunnel` with `{ "generation": number }`

All require relay DPoP authorization and an exact owner match. Tunnel release
uses its allocation generation so an old shutdown cannot delete a concurrent
relink.

### `POST /v1/environments/:id/connect`

Request: `{ "client_nonce": "random-base64url" }`.

The relay validates the managed allocation, signs a two-minute mint proof that
binds its issuer, the owner, environment ID, endpoint, nonce and client JWK
thumbprint, and sends it only to the configured managed endpoint at
`POST /v1/connect/mint`. It verifies the environment-signed response against
the pinned environment key before returning:

```json
{
  "environment_id": "uuid",
  "endpoint": {
    "http_base_url": "https://prod-digest.clippy.saudecomalex.com",
    "ws_base_url": "wss://prod-digest.clippy.saudecomalex.com"
  },
  "bootstrap_credential": "one-use-random-value",
  "expires_at": "2026-08-09T20:02:00Z"
}
```

The relay does not persist the bootstrap credential.

## Environment API

### `POST /v1/connect/mint`

Accepts only a valid Ed25519 relay proof for the pinned relay issuer and owner.
The proof is replay-protected and bound to the exact managed endpoint and
client JWK thumbprint. The Mac creates a random, one-use, two-minute bootstrap
credential and returns it with an Ed25519 signature over the complete response.

### `POST /v1/connect/token`

The iPhone sends the bootstrap credential plus a fresh DPoP proof. The Mac
consumes the credential once, verifies the proof key matches the relay-bound
thumbprint, and issues a one-hour environment access token:

```json
{
  "access_token": "random-environment-session-token",
  "token_type": "DPoP",
  "expires_in": 3600,
  "scope": "sync:read sync:write files:read files:write"
}
```

### `POST /v1/connect/websocket-ticket`

Requires the environment token and DPoP proof. Returns a single-use five-minute
ticket. The WebSocket URL contains only `?wsTicket=...`; access tokens never
appear in URLs.

Sync HTTP routes require the environment token and a request-specific DPoP
proof. Pairing additionally carries the existing WorkOS-authenticated principal
inside the relay-bound session so the workspace E2EE grant remains tied to the
same account. Existing ChaCha20-Poly1305 envelopes and content-addressed files
remain unchanged.
