import { env } from "cloudflare:workers";
import { applyD1Migrations } from "cloudflare:test";
import { beforeAll, describe, expect, it } from "vitest";
import { SignJWT, calculateJwkThumbprint, exportJWK, importJWK, jwtVerify, type JWK } from "jose";
import { canonicalJson } from "../src/canonical";
import {
  base64url,
  assertManagedHostname,
  deriveAllocatedHostname,
  issueRelayToken,
  signMintProof,
  sha256Base64url,
  verifyDpopProof,
  verifyEnvironmentSignature,
  verifyRelayToken,
} from "../src/crypto";
import { consumeLinkChallenge, createLinkChallenge, loadLinkChallenge } from "../src/db";

beforeAll(async () => {
  await applyD1Migrations(env.DB, env.TEST_MIGRATIONS);
});

describe("canonical signing", () => {
  it("orders nested object keys without changing array order", () => {
    expect(canonicalJson({ z: [3, { b: true, a: null }], a: "value" })).toBe(
      '{"a":"value","z":[3,{"a":null,"b":true}]}',
    );
  });

  it("verifies an Ed25519 environment signature over canonical JSON", async () => {
    const keyPair = await crypto.subtle.generateKey("Ed25519", true, ["sign", "verify"]);
    const publicJwk = (await exportJWK(keyPair.publicKey)) as JWK;
    const signedValue = { environment_id: "env-a", challenge: "nonce", issued_at: 123 };
    const signature = await crypto.subtle.sign(
      "Ed25519",
      keyPair.privateKey,
      new TextEncoder().encode(canonicalJson(signedValue)),
    );
    const thumbprint = await verifyEnvironmentSignature(
      publicJwk,
      signedValue,
      base64url(new Uint8Array(signature)),
    );
    expect(thumbprint).toBe(await calculateJwkThumbprint(publicJwk, "sha256"));
  });
});

describe("private hostname allocation", () => {
  it("is stable, stage-isolated, lowercase, and does not expose the environment id", async () => {
    const environmentId = "e91f20f1-5d2c-4ec4-80e9-48bd40cb7741";
    const staging = await deriveAllocatedHostname(
      "https://relay-staging.example.test",
      "clippy-staging.saudecomalex.com",
      "user_123",
      environmentId,
    );
    const again = await deriveAllocatedHostname(
      "https://relay-staging.example.test",
      "clippy-staging.saudecomalex.com",
      "user_123",
      environmentId,
    );
    const production = await deriveAllocatedHostname(
      "https://relay.example.test",
      "clippy.saudecomalex.com",
      "user_123",
      environmentId,
    );
    expect(staging).toMatch(/^staging-[a-f0-9]{32}\.clippy-staging\.saudecomalex\.com$/);
    expect(staging).toBe(again);
    expect(staging).not.toContain(environmentId);
    expect(production).not.toBe(staging);
  });

  it("rejects arbitrary and legacy-exact hosts as normal allocations", () => {
    expect(() =>
      assertManagedHostname("attacker.example", "clippy.saudecomalex.com"),
    ).toThrowError();
    expect(() =>
      assertManagedHostname("clippy.saudecomalex.com", "clippy.saudecomalex.com"),
    ).toThrowError();
    expect(
      assertManagedHostname("clippy.saudecomalex.com", "clippy.saudecomalex.com", {
        allowLegacy: true,
      }),
    ).toBe("clippy.saudecomalex.com");
  });
});

describe("one-use persistence", () => {
  it("consumes link challenges atomically", async () => {
    const identity = { sub: `user-${crypto.randomUUID()}`, orgId: "org-test" };
    const created = await createLinkChallenge(env.DB, identity, "env-test", "Test Mac");
    const loaded = await loadLinkChallenge(env.DB, created.challenge_id, identity);
    expect(loaded.challenge).toBe(created.challenge);
    await consumeLinkChallenge(env.DB, created.challenge_id);
    await expect(consumeLinkChallenge(env.DB, created.challenge_id)).rejects.toMatchObject({
      code: "link_challenge_replayed",
    });
  });

  it("persists DPoP jti and rejects replay", async () => {
    const accessToken = `access-${crypto.randomUUID()}`;
    const url = "https://relay-staging.example.test/v1/environments?ignored=query";
    const { proof } = await createDpopProof("GET", url, accessToken);
    const request = () => new Request(url, { headers: { dpop: proof } });
    await expect(verifyDpopProof(request(), env.DB, accessToken)).resolves.toHaveProperty("jkt");
    await expect(verifyDpopProof(request(), env.DB, accessToken)).rejects.toMatchObject({
      code: "dpop_replay",
    });
  });

  it("rejects a DPoP proof bound to another access token", async () => {
    const url = "https://relay-staging.example.test/v1/environments";
    const { proof } = await createDpopProof("GET", url, "first-token");
    await expect(
      verifyDpopProof(new Request(url, { headers: { dpop: proof } }), env.DB, "second-token"),
    ).rejects.toMatchObject({ code: "dpop_token_mismatch" });
  });
});

describe("relay access tokens", () => {
  it("includes scope and is bound to the DPoP thumbprint", async () => {
    const jkt = "client-jwk-thumbprint";
    const token = await issueRelayToken(env, { sub: "user-token", orgId: "org-token" }, jkt);
    expect(token.scope).toBe("relay:environments");
    expect(token.cnf.jkt).toBe(jkt);
    const verified = await verifyRelayToken(
      new Request("https://relay-staging.example.test/v1/environments", {
        headers: { authorization: `DPoP ${token.access_token}` },
      }),
      env,
    );
    expect(verified).toMatchObject({ sub: "user-token", orgId: "org-token", jkt });
  });

  it("keeps personal sessions owner-scoped when WorkOS omits org_id", async () => {
    const jkt = "personal-client-jwk-thumbprint";
    const token = await issueRelayToken(env, { sub: "personal-user", orgId: "" }, jkt);
    const verified = await verifyRelayToken(
      new Request("https://relay-staging.example.test/v1/environments", {
        headers: { authorization: `DPoP ${token.access_token}` },
      }),
      env,
    );
    expect(verified).toMatchObject({ sub: "personal-user", orgId: "", jkt });
  });

  it("signs the exact endpoint object and client key into an EdDSA mint JWT", async () => {
    const keyPair = await crypto.subtle.generateKey("Ed25519", true, ["sign", "verify"]);
    const privateJwk = await exportJWK(keyPair.privateKey);
    const publicJwk = await exportJWK(keyPair.publicKey);
    const signingEnv = { ...env, RELAY_SIGNING_PRIVATE_JWK: JSON.stringify(privateJwk) };
    const endpoint = {
      http_base_url: "https://prod-0123456789abcdef0123456789abcdef.clippy.saudecomalex.com",
      ws_base_url: "wss://prod-0123456789abcdef0123456789abcdef.clippy.saudecomalex.com",
    };
    const { proof } = await signMintProof(signingEnv, {
      ownerSub: "user-mint",
      orgId: "org-mint",
      environmentId: "environment-mint",
      endpoint,
      clientJkt: "client-thumbprint",
      clientNonce: "abcdefghijklmnop",
      generation: 7,
    });
    const key = await importJWK(publicJwk, "EdDSA");
    const verified = await jwtVerify(proof, key, {
      algorithms: ["EdDSA"],
      issuer: env.RELAY_ISSUER,
      audience: "clippy-env:environment-mint",
    });
    expect(verified.payload).toMatchObject({
      sub: "user-mint",
      org_id: "org-mint",
      environment_id: "environment-mint",
      endpoint,
      client_jkt: "client-thumbprint",
      client_nonce: "abcdefghijklmnop",
      generation: 7,
    });
  });
});

async function createDpopProof(
  method: string,
  url: string,
  accessToken: string,
): Promise<{ proof: string; jkt: string }> {
  const keyPair = await crypto.subtle.generateKey(
    { name: "ECDSA", namedCurve: "P-256" },
    true,
    ["sign", "verify"],
  );
  const publicJwk = await exportJWK(keyPair.publicKey);
  const jkt = await calculateJwkThumbprint(publicJwk, "sha256");
  const target = new URL(url);
  target.search = "";
  target.hash = "";
  const proof = await new SignJWT({
    htm: method,
    htu: target.toString(),
    ath: await sha256Base64url(accessToken),
  })
    .setProtectedHeader({ alg: "ES256", typ: "dpop+jwt", jwk: publicJwk })
    .setJti(crypto.randomUUID())
    .setIssuedAt()
    .sign(keyPair.privateKey);
  return { proof, jkt };
}
