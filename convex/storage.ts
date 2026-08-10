"use node";

import { S3Client, GetObjectCommand, HeadObjectCommand, PutObjectCommand } from "@aws-sdk/client-s3";
import { getSignedUrl } from "@aws-sdk/s3-request-presigner";
import { ConvexError, v } from "convex/values";
import { action } from "./_generated/server";
import { requireIdentity, requireUuid } from "./shared";

const MAX_OBJECTS_PER_CALL = 64;
const URL_TTL_SECONDS = 5 * 60;
let cachedR2: ReturnType<typeof createR2> | undefined;

function createR2() {
  const accountId = process.env.R2_ACCOUNT_ID;
  const bucket = process.env.R2_BUCKET;
  const accessKeyId = process.env.R2_ACCESS_KEY_ID;
  const secretAccessKey = process.env.R2_SECRET_ACCESS_KEY;
  if (!accountId || !bucket || !accessKeyId || !secretAccessKey) {
    throw new ConvexError("R2 is not configured");
  }
  return {
    bucket,
    client: new S3Client({
      region: "auto",
      endpoint: `https://${accountId}.r2.cloudflarestorage.com`,
      credentials: { accessKeyId, secretAccessKey },
    }),
  };
}

function r2() {
  cachedR2 ??= createR2();
  return cachedR2;
}

function requireHashes(hashes: string[]) {
  if (!hashes.length || hashes.length > MAX_OBJECTS_PER_CALL ||
      new Set(hashes).size !== hashes.length ||
      hashes.some((hash) => !/^[0-9a-f]{64}$/.test(hash))) {
    throw new ConvexError("Invalid object hash list");
  }
}

async function ownerPrefix(tokenIdentifier: string) {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(tokenIdentifier));
  return Buffer.from(digest).toString("hex");
}

export const prepareUploads = action({
  args: { workspaceId: v.string(), hashes: v.array(v.string()) },
  handler: async (ctx, args) => {
    const identity = await requireIdentity(ctx);
    requireUuid(args.workspaceId, "workspaceId");
    requireHashes(args.hashes);
    const { bucket, client } = r2();
    const prefix = await ownerPrefix(identity.tokenIdentifier);
    const results = await Promise.all(args.hashes.map(async (hash) => {
      const key = `v1/${prefix}/${args.workspaceId}/${hash}.e2ee`;
      try {
        await client.send(new HeadObjectCommand({ Bucket: bucket, Key: key }));
        return { hash, exists: true as const };
      } catch (error: any) {
        const status = error?.$metadata?.httpStatusCode;
        if (status !== 404 && error?.name !== "NotFound" && error?.name !== "NoSuchKey") throw error;
        const url = await getSignedUrl(
          client,
          new PutObjectCommand({ Bucket: bucket, Key: key, ContentType: "application/octet-stream" }),
          { expiresIn: URL_TTL_SECONDS },
        );
        return { hash, exists: false as const, url };
      }
    }));
    return results;
  },
});

export const downloadUrls = action({
  args: { workspaceId: v.string(), hashes: v.array(v.string()) },
  handler: async (ctx, args) => {
    const identity = await requireIdentity(ctx);
    requireUuid(args.workspaceId, "workspaceId");
    requireHashes(args.hashes);
    const { bucket, client } = r2();
    const prefix = await ownerPrefix(identity.tokenIdentifier);
    return await Promise.all(args.hashes.map(async (hash) => {
      const key = `v1/${prefix}/${args.workspaceId}/${hash}.e2ee`;
      const url = await getSignedUrl(
        client,
        new GetObjectCommand({ Bucket: bucket, Key: key }),
        { expiresIn: URL_TTL_SECONDS },
      );
      return { hash, url };
    }));
  },
});
