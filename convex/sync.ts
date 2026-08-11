import { ConvexError, v } from "convex/values";
import { mutation, query } from "./_generated/server";
import {
  ENROLLMENT_TTL_MS,
  MAX_BATCHES_PER_PULL,
  MAX_BATCH_CIPHERTEXT_CHARS,
  MAX_DEVICES,
  requireIdentity,
  requireOwnedWorkspace,
  requireRegisteredDevice,
  requireSafeText,
  requireUuid,
} from "./shared";

const envelope = v.object({
  version: v.number(),
  nonce: v.string(),
  ciphertext: v.string(),
});

const offer = v.object({
  version: v.number(),
  workspaceId: v.string(),
  syncUrl: v.string(),
  workosIssuer: v.string(),
  workosAudience: v.string(),
  macPublicKey: v.string(),
  oneTimeToken: v.string(),
  expiresAtMs: v.number(),
});

const grant = v.object({
  macPublicKey: v.string(),
  phonePublicKey: v.string(),
  sealedWorkspace: envelope,
});

export const bootstrap = mutation({
  args: {
    workspaceId: v.string(),
    actorId: v.string(),
    deviceName: v.string(),
    platform: v.string(),
  },
  handler: async (ctx, args) => {
    const identity = await requireIdentity(ctx);
    requireUuid(args.workspaceId, "workspaceId");
    requireUuid(args.actorId, "actorId");
    requireSafeText(args.deviceName, "deviceName", 160);
    requireSafeText(args.platform, "platform", 32);

    const byOwner = await ctx.db
      .query("workspaces")
      .withIndex("by_owner", (q) => q.eq("ownerId", identity.tokenIdentifier))
      .unique();
    const byId = await ctx.db
      .query("workspaces")
      .withIndex("by_workspace", (q) => q.eq("workspaceId", args.workspaceId))
      .unique();
    if (byOwner && byOwner.workspaceId !== args.workspaceId) {
      throw new ConvexError("This account already owns another workspace");
    }
    if (byId && byId.ownerId !== identity.tokenIdentifier) {
      throw new ConvexError("Workspace is owned by another account");
    }
    if (!byId) {
      await ctx.db.insert("workspaces", {
        workspaceId: args.workspaceId,
        ownerId: identity.tokenIdentifier,
        schemaVersion: 1,
        createdAt: Date.now(),
      });
    }

    let device = await ctx.db
      .query("devices")
      .withIndex("by_workspace_actor", (q: any) =>
        q.eq("workspaceId", args.workspaceId).eq("actorId", args.actorId),
      )
      .unique();
    if (!device) {
      const devices = await ctx.db
        .query("devices")
        .withIndex("by_workspace", (q) => q.eq("workspaceId", args.workspaceId))
        .take(MAX_DEVICES + 1);
      if (devices.length >= MAX_DEVICES) throw new ConvexError("Workspace device limit reached");
      const id = await ctx.db.insert("devices", {
        workspaceId: args.workspaceId,
        ownerId: identity.tokenIdentifier,
        actorId: args.actorId,
        name: args.deviceName,
        platform: args.platform,
        latestCounter: 0,
        lastSeenAt: Date.now(),
      });
      device = await ctx.db.get(id);
    } else if (device.ownerId !== identity.tokenIdentifier) {
      throw new ConvexError("Device is owned by another account");
    } else if (device.name !== args.deviceName || device.platform !== args.platform) {
      await ctx.db.patch(device._id, {
        name: args.deviceName,
        platform: args.platform,
      });
    }
    return { workspaceId: args.workspaceId, actorId: args.actorId };
  },
});

export const accountWorkspace = query({
  args: {},
  handler: async (ctx) => {
    const identity = await requireIdentity(ctx);
    const workspace = await ctx.db
      .query("workspaces")
      .withIndex("by_owner", (q) => q.eq("ownerId", identity.tokenIdentifier))
      .unique();
    return workspace ? { workspaceId: workspace.workspaceId } : null;
  },
});

export const changes = query({
  args: { workspaceId: v.string(), actorId: v.string() },
  handler: async (ctx, args) => {
    const identity = await requireIdentity(ctx);
    await requireOwnedWorkspace(ctx, args.workspaceId, identity.tokenIdentifier);
    await requireRegisteredDevice(ctx, args.workspaceId, args.actorId, identity.tokenIdentifier);
    const devices = await ctx.db
      .query("devices")
      .withIndex("by_workspace", (q) => q.eq("workspaceId", args.workspaceId))
      .take(MAX_DEVICES);
    return devices
      .map((device) => ({ actorId: device.actorId, latestCounter: device.latestCounter }))
      .sort((left, right) => left.actorId.localeCompare(right.actorId));
  },
});

export const deviceRegistration = query({
  args: { workspaceId: v.string(), actorId: v.string() },
  handler: async (ctx, args) => {
    const identity = await requireIdentity(ctx);
    requireUuid(args.workspaceId, "workspaceId");
    requireUuid(args.actorId, "actorId");
    await requireOwnedWorkspace(ctx, args.workspaceId, identity.tokenIdentifier);
    const device = await ctx.db
      .query("devices")
      .withIndex("by_workspace_actor", (q: any) =>
        q.eq("workspaceId", args.workspaceId).eq("actorId", args.actorId),
      )
      .unique();
    return { enrolled: device?.ownerId === identity.tokenIdentifier };
  },
});

// Every enrolled client subscribes to this narrow coordination query.
// Enrollment requests must wake a peer immediately even though they do not
// change device counters.
export const coordinationSignals = query({
  args: { workspaceId: v.string(), actorId: v.string() },
  handler: async (ctx, args) => {
    const identity = await requireIdentity(ctx);
    await requireOwnedWorkspace(ctx, args.workspaceId, identity.tokenIdentifier);
    await requireRegisteredDevice(ctx, args.workspaceId, args.actorId, identity.tokenIdentifier);
    const devices = await ctx.db
      .query("devices")
      .withIndex("by_workspace", (q) => q.eq("workspaceId", args.workspaceId))
      .take(MAX_DEVICES);
    const pending = await ctx.db
      .query("enrollments")
      .withIndex("by_workspace_status_expiry", (q: any) =>
        q.eq("workspaceId", args.workspaceId).eq("status", "pending"),
      )
      .first();
    return {
      counters: devices
        .map((device) => ({ actorId: device.actorId, latestCounter: device.latestCounter }))
        .sort((left, right) => left.actorId.localeCompare(right.actorId)),
      pendingEnrollmentId: pending?.enrollmentId ?? null,
    };
  },
});

export const push = mutation({
  args: {
    workspaceId: v.string(),
    actorId: v.string(),
    firstCounter: v.number(),
    lastCounter: v.number(),
    envelope,
  },
  handler: async (ctx, args) => {
    const identity = await requireIdentity(ctx);
    requireUuid(args.workspaceId, "workspaceId");
    requireUuid(args.actorId, "actorId");
    if (!Number.isSafeInteger(args.firstCounter) || !Number.isSafeInteger(args.lastCounter) ||
        args.firstCounter <= 0 || args.lastCounter < args.firstCounter) {
      throw new ConvexError("Invalid counter range");
    }
    if (args.envelope.version !== 1 || args.envelope.nonce.length > 64 ||
        args.envelope.ciphertext.length > MAX_BATCH_CIPHERTEXT_CHARS) {
      throw new ConvexError("Encrypted batch is too large or invalid");
    }
    await requireOwnedWorkspace(ctx, args.workspaceId, identity.tokenIdentifier);
    const device = await requireRegisteredDevice(
      ctx,
      args.workspaceId,
      args.actorId,
      identity.tokenIdentifier,
    );

    if (args.lastCounter <= device.latestCounter) {
      const existing = await ctx.db
        .query("operationBatches")
        .withIndex("by_workspace_actor_counter", (q: any) =>
          q.eq("workspaceId", args.workspaceId)
            .eq("actorId", args.actorId)
            .eq("firstCounter", args.firstCounter),
        )
        .unique();
      if (existing?.lastCounter === args.lastCounter) {
        return { acceptedThrough: device.latestCounter, duplicate: true };
      }
      throw new ConvexError("Counter range overlaps existing data");
    }
    if (args.firstCounter !== device.latestCounter + 1) {
      throw new ConvexError("Counter range is not contiguous");
    }
    await ctx.db.insert("operationBatches", {
      workspaceId: args.workspaceId,
      ownerId: identity.tokenIdentifier,
      actorId: args.actorId,
      firstCounter: args.firstCounter,
      lastCounter: args.lastCounter,
      envelope: args.envelope,
      createdAt: Date.now(),
    });
    await ctx.db.patch(device._id, {
      latestCounter: args.lastCounter,
      lastSeenAt: Date.now(),
    });
    return { acceptedThrough: args.lastCounter, duplicate: false };
  },
});

export const pull = query({
  args: {
    workspaceId: v.string(),
    actorId: v.string(),
    frontier: v.array(v.object({ actorId: v.string(), counter: v.number() })),
  },
  handler: async (ctx, args) => {
    const identity = await requireIdentity(ctx);
    await requireOwnedWorkspace(ctx, args.workspaceId, identity.tokenIdentifier);
    await requireRegisteredDevice(ctx, args.workspaceId, args.actorId, identity.tokenIdentifier);
    if (args.frontier.length > MAX_DEVICES) throw new ConvexError("Frontier is too large");
    const frontier = new Map(args.frontier.map((entry) => [entry.actorId, entry.counter]));
    const devices = await ctx.db
      .query("devices")
      .withIndex("by_workspace", (q) => q.eq("workspaceId", args.workspaceId))
      .take(MAX_DEVICES);
    const batches = [];
    for (const device of devices) {
      if (batches.length >= MAX_BATCHES_PER_PULL) break;
      const after = frontier.get(device.actorId) ?? 0;
      const remaining = MAX_BATCHES_PER_PULL - batches.length;
      const actorBatches = await ctx.db
        .query("operationBatches")
        .withIndex("by_workspace_actor_counter", (q: any) =>
          q.eq("workspaceId", args.workspaceId)
            .eq("actorId", device.actorId)
            .gt("firstCounter", after),
        )
        .take(remaining);
      batches.push(...actorBatches.map((batch) => ({
        actorId: batch.actorId,
        firstCounter: batch.firstCounter,
        lastCounter: batch.lastCounter,
        envelope: batch.envelope,
      })));
    }
    batches.sort((left, right) =>
      left.firstCounter - right.firstCounter || left.actorId.localeCompare(right.actorId),
    );
    return batches;
  },
});

export const requestEnrollment = mutation({
  args: {
    enrollmentId: v.string(),
    actorId: v.string(),
    deviceName: v.string(),
    phonePublicKey: v.string(),
    platform: v.optional(v.string()),
    recoverKey: v.optional(v.boolean()),
  },
  handler: async (ctx, args) => {
    const identity = await requireIdentity(ctx);
    requireUuid(args.enrollmentId, "enrollmentId");
    requireUuid(args.actorId, "actorId");
    requireSafeText(args.deviceName, "deviceName", 160);
    requireSafeText(args.phonePublicKey, "phonePublicKey", 128);
    const platform = args.platform ?? "ios";
    requireSafeText(platform, "platform", 32);
    const workspace = await ctx.db
      .query("workspaces")
      .withIndex("by_owner", (q) => q.eq("ownerId", identity.tokenIdentifier))
      .unique();
    if (!workspace) return { state: "noWorkspace" as const };
    const existingDevice = await ctx.db
      .query("devices")
      .withIndex("by_workspace_actor", (q: any) =>
        q.eq("workspaceId", workspace.workspaceId).eq("actorId", args.actorId),
      )
      .unique();
    if (existingDevice && !args.recoverKey) {
      return { state: "alreadyEnrolled" as const, workspaceId: workspace.workspaceId };
    }
    const existing = await ctx.db
      .query("enrollments")
      .withIndex("by_enrollment", (q) => q.eq("enrollmentId", args.enrollmentId))
      .unique();
    if (existing) {
      if (existing.ownerId !== identity.tokenIdentifier || existing.actorId !== args.actorId) {
        throw new ConvexError("Enrollment identifier collision");
      }
      return { state: existing.status, workspaceId: existing.workspaceId };
    }
    await ctx.db.insert("enrollments", {
      enrollmentId: args.enrollmentId,
      workspaceId: workspace.workspaceId,
      ownerId: identity.tokenIdentifier,
      actorId: args.actorId,
      deviceName: args.deviceName,
      phonePublicKey: args.phonePublicKey,
      platform,
      status: "pending",
      expiresAt: Date.now() + ENROLLMENT_TTL_MS,
      createdAt: Date.now(),
    });
    return { state: "pending" as const, workspaceId: workspace.workspaceId };
  },
});

export const pendingEnrollments = query({
  args: { workspaceId: v.string(), actorId: v.string() },
  handler: async (ctx, args) => {
    const identity = await requireIdentity(ctx);
    await requireOwnedWorkspace(ctx, args.workspaceId, identity.tokenIdentifier);
    await requireRegisteredDevice(ctx, args.workspaceId, args.actorId, identity.tokenIdentifier);
    const pending = await ctx.db
      .query("enrollments")
      .withIndex("by_workspace_status_expiry", (q: any) =>
        q.eq("workspaceId", args.workspaceId)
          .eq("status", "pending")
          .gt("expiresAt", Date.now()),
      )
      .take(16);
    return pending
      .map((entry) => ({
        enrollmentId: entry.enrollmentId,
        actorId: entry.actorId,
        deviceName: entry.deviceName,
        phonePublicKey: entry.phonePublicKey,
        expiresAt: entry.expiresAt,
      }));
  },
});

export const grantEnrollment = mutation({
  args: {
    workspaceId: v.string(),
    actorId: v.string(),
    enrollmentId: v.string(),
    offer,
    grant,
  },
  handler: async (ctx, args) => {
    const identity = await requireIdentity(ctx);
    await requireOwnedWorkspace(ctx, args.workspaceId, identity.tokenIdentifier);
    await requireRegisteredDevice(ctx, args.workspaceId, args.actorId, identity.tokenIdentifier);
    const enrollment = await ctx.db
      .query("enrollments")
      .withIndex("by_enrollment", (q) => q.eq("enrollmentId", args.enrollmentId))
      .unique();
    if (!enrollment || enrollment.ownerId !== identity.tokenIdentifier ||
        enrollment.workspaceId !== args.workspaceId) {
      throw new ConvexError("Enrollment is unavailable");
    }
    if (enrollment.status === "granted" || enrollment.status === "accepted") {
      return { granted: false };
    }
    if (enrollment.expiresAt < Date.now()) throw new ConvexError("Enrollment is unavailable");
    if (args.offer.workspaceId !== args.workspaceId || args.offer.expiresAtMs < Date.now() ||
        args.grant.phonePublicKey !== enrollment.phonePublicKey ||
        args.grant.macPublicKey !== args.offer.macPublicKey) {
      throw new ConvexError("Enrollment grant does not match its request");
    }
    await ctx.db.patch(enrollment._id, { status: "granted", offer: args.offer, grant: args.grant });
    return { granted: true };
  },
});

export const enrollmentStatus = query({
  args: { enrollmentId: v.string(), actorId: v.string() },
  handler: async (ctx, args) => {
    const identity = await requireIdentity(ctx);
    const enrollment = await ctx.db
      .query("enrollments")
      .withIndex("by_enrollment", (q) => q.eq("enrollmentId", args.enrollmentId))
      .unique();
    if (!enrollment || enrollment.ownerId !== identity.tokenIdentifier ||
        enrollment.actorId !== args.actorId) return null;
    if (enrollment.status === "accepted") {
      return { state: "accepted" as const, workspaceId: enrollment.workspaceId };
    }
    if (enrollment.expiresAt < Date.now()) return { state: "expired" as const };
    if (enrollment.status === "granted") {
      return {
        state: "granted" as const,
        workspaceId: enrollment.workspaceId,
        offer: enrollment.offer,
        grant: enrollment.grant,
      };
    }
    return { state: enrollment.status };
  },
});

export const acceptEnrollment = mutation({
  args: { enrollmentId: v.string(), actorId: v.string() },
  handler: async (ctx, args) => {
    const identity = await requireIdentity(ctx);
    const enrollment = await ctx.db
      .query("enrollments")
      .withIndex("by_enrollment", (q) => q.eq("enrollmentId", args.enrollmentId))
      .unique();
    if (!enrollment || enrollment.ownerId !== identity.tokenIdentifier ||
        enrollment.actorId !== args.actorId) {
      throw new ConvexError("Enrollment is unavailable");
    }
    if (enrollment.status === "accepted") {
      const acceptedDevice = await ctx.db
        .query("devices")
        .withIndex("by_workspace_actor", (q: any) =>
          q.eq("workspaceId", enrollment.workspaceId).eq("actorId", args.actorId),
        )
        .unique();
      if (!acceptedDevice || acceptedDevice.ownerId !== identity.tokenIdentifier) {
        throw new ConvexError("Accepted enrollment has no device");
      }
      return { workspaceId: enrollment.workspaceId, actorId: acceptedDevice.actorId };
    }
    if (enrollment.status !== "granted" || enrollment.expiresAt < Date.now()) {
      throw new ConvexError("Enrollment is unavailable");
    }
    let device = await ctx.db
      .query("devices")
      .withIndex("by_workspace_actor", (q: any) =>
        q.eq("workspaceId", enrollment.workspaceId).eq("actorId", args.actorId),
      )
      .unique();
    if (!device) {
      const devices = await ctx.db
        .query("devices")
        .withIndex("by_workspace", (q) => q.eq("workspaceId", enrollment.workspaceId))
        .take(MAX_DEVICES + 1);
      if (devices.length >= MAX_DEVICES) throw new ConvexError("Workspace device limit reached");
      const id = await ctx.db.insert("devices", {
        workspaceId: enrollment.workspaceId,
        ownerId: identity.tokenIdentifier,
        actorId: args.actorId,
        name: enrollment.deviceName,
        platform: enrollment.platform ?? "ios",
        latestCounter: 0,
        lastSeenAt: Date.now(),
      });
      device = await ctx.db.get(id);
    }
    await ctx.db.patch(enrollment._id, { status: "accepted" });
    const platform = enrollment.platform ?? device!.platform;
    if (device!.name !== enrollment.deviceName || device!.platform !== platform) {
      await ctx.db.patch(device!._id, {
        name: enrollment.deviceName,
        platform,
      });
    }
    return { workspaceId: enrollment.workspaceId, actorId: device!.actorId };
  },
});
