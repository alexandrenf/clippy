import type { AuthConfig } from "convex/server";

const issuer = process.env.WORKOS_ISSUER?.replace(/\/$/, "");
const clientId = process.env.WORKOS_CLIENT_ID;

if (!issuer || !clientId) {
  throw new Error("Set WORKOS_ISSUER and WORKOS_CLIENT_ID in this Convex deployment");
}

export default {
  providers: [
    {
      type: "customJwt",
      issuer,
      applicationID: clientId,
      jwks: `${issuer}/oauth2/jwks`,
      algorithm: "RS256",
    },
  ],
} satisfies AuthConfig;
