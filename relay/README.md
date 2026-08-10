# Clippy relay control plane

This directory contains the Cloudflare Worker that authenticates Clippy accounts,
links Mac environments, provisions remotely managed Cloudflare Tunnels, and brokers
one short-lived bootstrap credential. It is deliberately **not** in the normal sync
or file-transfer data path.

The implementation is isolated from the Mac, iOS, and desktop app sources. The
machine-readable wire contract is [`openapi.json`](./openapi.json).

## Security model

- WorkOS access tokens are accepted only as AuthKit RS256 JWTs with the
  configured exact issuer and audience, a valid expiry, and a non-empty `sub`.
  `org_id` is enforced when present; personal sessions remain owner-scoped by
  `sub`. JWKS are fetched from the issuer's `/oauth2/jwks` endpoint and cached.
- Every request also carries a fresh ES256 DPoP proof. The proof embeds a public
  P-256 JWK and binds `htm`, normalized `htu`, `iat`, unique `jti`, and `ath` to
  the presented access token. Used JTIs are persisted in D1.
- Relay tokens live for five minutes, contain `scope: relay:environments`, and
  are bound to the client's JWK thumbprint in `cnf.jkt`.
- An environment link challenge is valid for five minutes and atomically consumed
  once. The Mac signs the exact canonical link object with its Ed25519 key; the
  relay permanently pins the owner subject, organization, and key thumbprint.
- The relay signs environment mint requests with a separate Ed25519 key. The
  corresponding public JWK is returned to the Mac only through the authenticated
  HTTPS link response so the Mac can pin it.
- Connector tokens and bootstrap credentials are returned only in their one
  required response. Neither is persisted or logged by the Worker.
- Outbound mint calls are constructed only from an allocated hostname beneath the
  configured stage suffix. Tunnel origins are always exactly
  `http://127.0.0.1:<ORIGIN_PORT>`, followed by an `http_status:404` catch-all.
  Redirects are rejected.

## Stage isolation and host allocation

Each stage has a separate Worker, D1 database, WorkOS audience, relay issuer, and
managed hostname suffix:

| Stage | Relay issuer | Allocation suffix | Loopback origin |
| --- | --- | --- | --- |
| staging | `https://relay-staging.saudecomalex.com` | `clippy-staging.saudecomalex.com` | `http://127.0.0.1:49832` |
| production | `https://relay.saudecomalex.com` | `clippy.saudecomalex.com` | `http://127.0.0.1:49833` |

The allocation is deterministic:

```text
<stage>-<first-128-bits-of-sha256(stage NUL owner_sub NUL environment_id)>.<suffix>
```

It is lowercase and never places a raw account or environment identifier in DNS.
The exact legacy hosts `clippy-staging.saudecomalex.com` and
`clippy.saudecomalex.com` are migration aliases only; they are never newly
allocated.

## Required configuration

Replace every `REPLACE_WITH_*` value in `wrangler.staging.jsonc` and
`wrangler.production.jsonc` before deploying.

Non-secret Worker variables:

- `WORKOS_ISSUER`
- `WORKOS_AUDIENCE`
- `RELAY_ISSUER`
- `PUBLIC_HOSTNAME`
- `ORIGIN_PORT` (`49832` for staging and `49833` for production)
- `CLOUDFLARE_ACCOUNT_ID`
- `CLOUDFLARE_ZONE_ID`

Secrets, configured separately for each Worker:

- `CLOUDFLARE_API_TOKEN` — use a narrowly scoped token with Cloudflare Tunnel
  edit access for the account and DNS edit access for the one zone.
- `RELAY_TOKEN_SECRET` — at least 32 cryptographically random bytes.
- `RELAY_SIGNING_PRIVATE_JWK` — an Ed25519 private JWK encoded as one JSON string.

Do not place any of these three secrets in a Wrangler file, shell history, source
control, CI logs, or test snapshots.

## Provisioning and deploy

Run these commands from this directory after authenticating Wrangler:

```sh
npm install
npx wrangler d1 create clippy-relay-staging
npx wrangler d1 create clippy-relay-production
```

Copy the returned D1 IDs into the matching Wrangler files, then configure each
secret with the matching `--config` value:

```sh
npx wrangler secret put CLOUDFLARE_API_TOKEN --config wrangler.staging.jsonc
npx wrangler secret put RELAY_TOKEN_SECRET --config wrangler.staging.jsonc
npx wrangler secret put RELAY_SIGNING_PRIVATE_JWK --config wrangler.staging.jsonc
```

Repeat for `wrangler.production.jsonc` using independent production secrets. Apply
migrations and deploy staging first:

```sh
npm run migrate:staging
npm run deploy:staging
npm run migrate:production
npm run deploy:production
```

The Worker API token is used only for these managed resources:

1. a remotely managed tunnel named from the opaque allocation hostname;
2. tunnel ingress containing the allocated host, approved legacy aliases, the
   configured loopback service, and a final 404 rule;
3. one proxied CNAME from the allocated hostname to `<tunnel-id>.cfargotunnel.com`;
4. retrieving the connector token for the link response.

Link retries reuse the named tunnel and DNS record. A short D1 provision lease
prevents concurrent links from allocating twice.

## Legacy hostname migration

Legacy aliases are opt-in D1 records. Associate an exact stage legacy hostname
with only the already-linked environment that previously owned it. If the alias
already has a Cloudflare DNS record, store that record ID so tunnel deletion can
clean it up safely.

```sql
INSERT INTO environment_hostname_aliases(
  environment_id, hostname, dns_record_id, is_legacy, created_at
) VALUES(
  '<environment-id>',
  'clippy-staging.saudecomalex.com',
  NULL,
  1,
  unixepoch()
);
```

On the next authenticated relink, the alias is added to that environment's tunnel
ingress. Do not use this table to allocate new environments.

## Wire contracts that must remain exact

All JSON fields are snake_case. Public endpoint values are always objects:

```json
{
  "http_base_url": "https://prod-opaque.clippy.saudecomalex.com",
  "ws_base_url": "wss://prod-opaque.clippy.saudecomalex.com"
}
```

Environment objects include `id`, `environment_id`, and `workspace_id`; all three
have the same value. `id` is retained for native-client compatibility.

The link signature covers RFC 8785 canonical JSON of exactly:

```json
{
  "challenge": "...",
  "challenge_id": "...",
  "environment_id": "...",
  "environment_public_jwk": { "kty": "OKP", "crv": "Ed25519", "x": "..." },
  "issued_at": "2026-08-09T20:00:00Z",
  "name": "Alexandre's Mac"
}
```

`POST /v1/environments/:id/connect` accepts only `{ "client_nonce": "..." }`.
The relay sends the environment `{ "proof": "<compact-EdDSA-JWT>" }`. Its claims
are `iss`, `aud=clippy-env:<environment_id>`, `sub`, optional `org_id`,
`environment_id`, the endpoint object, `client_jkt`, `client_nonce`, `generation`,
`jti`, `iat`, and `exp`.

The environment returns exactly:

```json
{
  "environment_id": "...",
  "bootstrap_credential": "...",
  "expires_at": "2026-08-09T20:02:00Z",
  "client_jkt": "...",
  "client_nonce": "...",
  "signature": "..."
}
```

Its Ed25519 signature covers canonical JSON of the other five fields. The relay
checks the pinned key, identity, nonce, client thumbprint, and a maximum five-minute
expiry before returning the credential, and never writes the credential to D1.

## Local verification

The tests run inside Cloudflare's Workers Vitest runtime and apply the D1 migration
to an isolated Miniflare database:

```sh
npm run check
npm test
```

They cover canonical Ed25519 verification, opaque stage-isolated hostname
allocation, one-use challenges, persisted DPoP replay rejection, access-token
binding, and token scope. No test uses live WorkOS or Cloudflare credentials.

Before production deployment, also exercise link/unlink against a dedicated test
zone and confirm the generated tunnel configuration has only the expected managed
hostname, loopback origin, and final 404 rule.
