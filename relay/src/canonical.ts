import { ApiError } from "./errors";

/** RFC 8785/JCS-compatible canonical JSON for JSON-only values. */
export function canonicalJson(value: unknown): string {
  if (value === null || typeof value === "boolean" || typeof value === "string") {
    return JSON.stringify(value);
  }
  if (typeof value === "number") {
    if (!Number.isFinite(value)) throw new ApiError(400, "invalid_number", "Numbers must be finite");
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (isRecord(value)) {
    return `{${Object.keys(value)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`)
      .join(",")}}`;
  }
  throw new ApiError(400, "invalid_canonical_value", "The signed value must contain only JSON data");
}

export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function requireString(
  value: unknown,
  field: string,
  options: { min?: number; max?: number; pattern?: RegExp } = {},
): string {
  if (typeof value !== "string") {
    throw new ApiError(400, "invalid_request", `${field} must be a string`);
  }
  const min = options.min ?? 1;
  const max = options.max ?? 256;
  if (value.length < min || value.length > max || (options.pattern && !options.pattern.test(value))) {
    throw new ApiError(400, "invalid_request", `${field} is not valid`);
  }
  return value;
}

export function requireInteger(value: unknown, field: string): number {
  if (!Number.isSafeInteger(value)) {
    throw new ApiError(400, "invalid_request", `${field} must be an integer`);
  }
  return value as number;
}
