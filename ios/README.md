# Clippy for iPhone

Open `ClippyMobile.xcodeproj` or regenerate it with `xcodegen generate` after
editing `project.yml`. The app targets iOS 17+ and iPhone bundle ID
`app.clippy.mobile`.

- `Clippy Staging` uses the committed public staging WorkOS issuer/client and
  allows environment endpoints under `clippy-staging.saudecomalex.com`.
- `Clippy Production` uses the separate production public client,
  `https://brave-mermaid-84.authkit.app`, and
  allows environment endpoints under `clippy.saudecomalex.com`.
- `RELAY_BASE_URL` is pinned to the environment-specific Workers at
  `relay-staging.saudecomalex.com` and `relay.saudecomalex.com`; malformed or
  empty values fail closed at launch.
- No API key, client secret, tunnel token, OAuth token, or encryption key belongs
  in an xcconfig file. Runtime tokens and workspace keys are Keychain-only.

Validation:

```sh
swift test --disable-sandbox
xcodebuild -project ClippyMobile.xcodeproj -scheme "Clippy Staging" \
  -configuration Staging -destination "generic/platform=iOS Simulator" \
  CODE_SIGNING_ALLOWED=NO build
```

The phone authenticates to the relay with its WorkOS access token and a
persistent P-256 DPoP key stored as `AfterFirstUnlockThisDeviceOnly`. The relay
issues a DPoP-bound relay token and then a one-use environment bootstrap. Sync
HTTP calls use the environment DPoP token; WebSockets use only a short one-use
query ticket, never a long-lived token in the URL or upgrade headers.

The existing X25519 key grant and ChaCha20-Poly1305 CRDT/chunk encryption
remain above that session layer. A device signed into the same account is
enrolled automatically after environment bootstrap, so
WorkOS and the relay never derive the workspace encryption key.

The companion includes a durable local item/op/chunk replica, explicit
multi-value content conflict resolution, and encrypted 1 MiB chunks capped at
250 MiB per attachment. It syncs only in the foreground. Offline mode owns no
retry timer; transient failures use a capped 1/2/4/8/16-second ladder, while
authentication/configuration failures remain blocked until a credential or
configuration wake. Camera QR scanning, APNs provisioning, device revocation,
and live-device staging/production acceptance remain follow-up work.
