import type { AuthConfig } from "convex/server";

const issuer = process.env.WORKOS_ISSUER?.replace(/\/$/, "");
const audience = process.env.WORKOS_AUDIENCE;

if (!issuer || !audience) {
  throw new Error("Set WORKOS_ISSUER and WORKOS_AUDIENCE in this Convex deployment");
}

export default {
  providers: [
    {
      type: "customJwt",
      issuer,
      applicationID: audience,
      jwks: `${issuer}/oauth2/jwks`,
      algorithm: "RS256",
    },
  ],
} satisfies AuthConfig;
