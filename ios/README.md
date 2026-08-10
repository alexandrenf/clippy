# Clippy for iPhone

The app targets iOS 17+ with bundle ID `app.clippy.mobile`. Open
`ClippyMobile.xcodeproj`, or regenerate it after editing `project.yml`:

```sh
cd ios
xcodegen generate --spec project.yml
```

`Clippy Staging` and `Clippy Production` use separate WorkOS and Convex public
routing values. Replace the placeholder `CONVEX_URL` in each environment's
xcconfig after creating the deployments. Never put WorkOS secrets, R2 keys,
OAuth tokens, or workspace encryption keys in an xcconfig. OAuth sessions are
stored in an app-private, file-protected, no-backup file so reads never trigger
a Keychain password prompt. Only the workspace encryption key uses
ThisDeviceOnly Keychain storage.

The phone uses the official Convex Swift client for an authenticated realtime
subscription and small mutation/query calls. Attachments bypass Convex and move
directly to the private R2 S3 endpoint with five-minute URLs. The existing
X25519 grant and ChaCha20-Poly1305 CRDT/chunk encryption remain above that
transport, so WorkOS, Convex, and R2 cannot derive the workspace key.

The app opens its Convex subscription only while foregrounded and online.
Local writes and reactive counter changes coalesce into one sync exchange;
transient failures use a capped 1/2/4/8/16-second retry ladder and a five-minute
safety pass. The local replica continues working offline.

Validation:

```sh
swift test
xcodebuild -project ClippyMobile.xcodeproj -scheme "Clippy Staging" \
  -configuration Staging -destination "generic/platform=iOS Simulator" \
  CODE_SIGNING_ALLOWED=NO build
```

Physical-device staging/production acceptance, APNs background wake, device
revocation, and workspace-key rotation remain follow-up work.
