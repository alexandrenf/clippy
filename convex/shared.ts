import { ConvexError } from "convex/values";

export const MAX_DEVICES = 32;
export const MAX_BATCHES_PER_PULL = 12;
export const MAX_BATCH_CIPHERTEXT_CHARS = 760_000;
export const ENROLLMENT_TTL_MS = 10 * 60 * 1_000;

export function requireUuid(value: string, field: string): void {
  if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(value)) {
    throw new ConvexError(`${field} must be a UUID`);
  }
}

export function requireSafeText(value: string, field: string, maxBytes: number): void {
  if (!value || new TextEncoder().encode(value).byteLength > maxBytes) {
    throw new ConvexError(`${field} is invalid`);
  }
}

export async function requireIdentity(ctx: { auth: { getUserIdentity(): Promise<{ tokenIdentifier: string } | null> } }) {
  const identity = await ctx.auth.getUserIdentity();
  if (!identity) throw new ConvexError("Authentication required");
  return identity;
}

export async function requireOwnedWorkspace(
  ctx: any,
  workspaceId: string,
  ownerId: string,
) {
  const workspace = await ctx.db
    .query("workspaces")
    .withIndex("by_workspace", (q: any) => q.eq("workspaceId", workspaceId))
    .unique();
  if (!workspace || workspace.ownerId !== ownerId) {
    throw new ConvexError("Workspace not found");
  }
  return workspace;
}

export async function requireRegisteredDevice(
  ctx: any,
  workspaceId: string,
  actorId: string,
  ownerId: string,
) {
  const device = await ctx.db
    .query("devices")
    .withIndex("by_workspace_actor", (q: any) =>
      q.eq("workspaceId", workspaceId).eq("actorId", actorId),
    )
    .unique();
  if (!device || device.ownerId !== ownerId) {
    throw new ConvexError("Device is not enrolled");
  }
  return device;
}
