import { defineSchema, defineTable } from "convex/server";
import { v } from "convex/values";

const envelope = v.object({
  version: v.number(),
  nonce: v.string(),
  ciphertext: v.string(),
});

export default defineSchema({
  workspaces: defineTable({
    workspaceId: v.string(),
    ownerId: v.string(),
    schemaVersion: v.number(),
    createdAt: v.number(),
  })
    .index("by_workspace", ["workspaceId"])
    .index("by_owner", ["ownerId"]),

  devices: defineTable({
    workspaceId: v.string(),
    ownerId: v.string(),
    actorId: v.string(),
    name: v.string(),
    platform: v.string(),
    latestCounter: v.number(),
    lastSeenAt: v.number(),
  })
    .index("by_workspace_actor", ["workspaceId", "actorId"])
    .index("by_workspace", ["workspaceId"]),

  operationBatches: defineTable({
    workspaceId: v.string(),
    ownerId: v.string(),
    actorId: v.string(),
    firstCounter: v.number(),
    lastCounter: v.number(),
    envelope,
    createdAt: v.number(),
  })
    .index("by_workspace_actor_counter", ["workspaceId", "actorId", "firstCounter"]),

  enrollments: defineTable({
    enrollmentId: v.string(),
    workspaceId: v.string(),
    ownerId: v.string(),
    actorId: v.string(),
    deviceName: v.string(),
    phonePublicKey: v.string(),
    platform: v.optional(v.string()),
    status: v.union(v.literal("pending"), v.literal("granted"), v.literal("accepted")),
    expiresAt: v.number(),
    offer: v.optional(v.any()),
    grant: v.optional(v.any()),
    createdAt: v.number(),
  })
    .index("by_enrollment", ["enrollmentId"])
    .index("by_workspace_status_expiry", ["workspaceId", "status", "expiresAt"]),
});
