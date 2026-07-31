import { HttpError } from "./errors.js";

export const SCHEMA_VERSION = "shuttle.v1";

export const CORS_HEADERS: Record<string, string> = {
  "access-control-allow-origin": "*",
  "access-control-allow-methods": "GET,POST,DELETE,OPTIONS",
  "access-control-allow-headers":
    "accept,authorization,content-type,cf-access-client-id,cf-access-client-secret,mcp-protocol-version,mcp-session-id",
  "access-control-expose-headers": "mcp-session-id",
};

export function json(value: unknown, status = 200): Response {
  const body =
    value && typeof value === "object" && !Array.isArray(value)
      ? { ...(value as Record<string, unknown>), schema_version: SCHEMA_VERSION }
      : {
          schema_version: SCHEMA_VERSION,
          items: value,
          pagination: { returned: Array.isArray(value) ? value.length : 1, has_more: false },
        };
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json", ...CORS_HEADERS },
  });
}

export function errorResponse(error: unknown): Response {
  if (error instanceof HttpError) {
    return json(
      {
        error: {
          code:
            error.status === 400
              ? "invalid_request"
              : error.status === 401
                ? "unauthorized"
                : error.status === 403
                  ? "forbidden"
                  : error.status === 404
                    ? "not_found"
                    : error.status === 409
                      ? "conflict"
                      : "request_failed",
          message: error.message,
          retryable: error.status === 429 || error.status >= 500,
        },
      },
      error.status,
    );
  }
  return json(
    {
      error: {
        code: "internal_error",
        message: "internal server error",
        retryable: true,
      },
    },
    500,
  );
}

export async function readJson(request: Request): Promise<Record<string, unknown>> {
  try {
    const body = await request.json();
    return body && typeof body === "object" ? (body as Record<string, unknown>) : {};
  } catch {
    return {};
  }
}
