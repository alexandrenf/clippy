export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
  ) {
    super(message);
  }
}

export function json(data: unknown, status = 200, headers?: HeadersInit): Response {
  return Response.json(data, {
    status,
    headers: {
      "cache-control": "no-store",
      ...headers,
    },
  });
}

export function errorResponse(error: unknown): Response {
  if (error instanceof ApiError) {
    return json({ error: { code: error.code, message: error.message } }, error.status);
  }
  // Log only the error class and source locations. Messages and serialized
  // values can contain bearer tokens, connector credentials, or signed proofs.
  const kind = error instanceof Error ? error.name : typeof error;
  const locations = error instanceof Error
    ? (error.stack ?? "")
        .split("\n")
        .slice(1, 4)
        .map((line) => line.match(/(?:src\/[^:)]+:\d+:\d+|index\.js:\d+:\d+)/)?.[0])
        .filter((line): line is string => Boolean(line))
    : [];
  console.error("relay_internal_error", { kind, locations });
  return json(
    { error: { code: "internal_error", message: "The relay could not complete the request" } },
    500,
  );
}

export async function readJson<T>(request: Request, maxBytes = 32_768): Promise<T> {
  const length = Number(request.headers.get("content-length") ?? "0");
  if (Number.isFinite(length) && length > maxBytes) {
    throw new ApiError(413, "body_too_large", "The JSON request body is too large");
  }
  const text = await readTextLimited(request.body, maxBytes);
  try {
    return JSON.parse(text) as T;
  } catch {
    throw new ApiError(400, "invalid_json", "The request body must be valid JSON");
  }
}

export async function readOptionalJson<T>(request: Request, maxBytes = 32_768): Promise<T | undefined> {
  const length = Number(request.headers.get("content-length") ?? "0");
  if (Number.isFinite(length) && length > maxBytes) {
    throw new ApiError(413, "body_too_large", "The JSON request body is too large");
  }
  const text = await readTextLimited(request.body, maxBytes);
  if (text.trim() === "") return undefined;
  try {
    return JSON.parse(text) as T;
  } catch {
    throw new ApiError(400, "invalid_json", "The request body must be valid JSON");
  }
}

export async function readResponseJson<T>(response: Response, maxBytes = 65_536): Promise<T> {
  const length = Number(response.headers.get("content-length") ?? "0");
  if (Number.isFinite(length) && length > maxBytes) {
    throw new ApiError(502, "environment_response_too_large", "The environment response is too large");
  }
  const text = await readTextLimited(response.body, maxBytes);
  try {
    return JSON.parse(text) as T;
  } catch {
    throw new ApiError(502, "invalid_environment_response", "The environment returned invalid JSON");
  }
}

async function readTextLimited(
  body: ReadableStream<Uint8Array> | null,
  maxBytes: number,
): Promise<string> {
  if (!body) return "";
  const reader = body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.byteLength;
    if (total > maxBytes) {
      await reader.cancel();
      throw new ApiError(413, "body_too_large", "The message is too large");
    }
    chunks.push(value);
  }
  const bytes = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return new TextDecoder().decode(bytes);
}
