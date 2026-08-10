// This small checked-in shim lets the backend typecheck before a Convex
// deployment exists. `npx convex dev` will replace it with generated types.
export {
  actionGeneric as action,
  mutationGeneric as mutation,
  queryGeneric as query,
} from "convex/server";
