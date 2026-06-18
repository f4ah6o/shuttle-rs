import { handleApi } from "./api.js";
import { authenticate } from "./auth.js";
import { D1Database_, type Database } from "./database.js";
import type { Env } from "./env.js";
import { CORS_HEADERS, errorResponse, json, readJson } from "./http.js";
import { handleMcp, type RpcRequest } from "./mcp.js";

export async function handle(request: Request, env: Env, db: Database): Promise<Response> {
  try {
    return await route(request, env, db);
  } catch (error) {
    return errorResponse(error);
  }
}

async function route(request: Request, env: Env, db: Database): Promise<Response> {
  const url = new URL(request.url);
  const path = url.pathname;

  if (request.method === "OPTIONS") {
    return new Response(null, { status: 204, headers: CORS_HEADERS });
  }

  // Health check requires no authentication.
  if (path === "/api/health" && request.method === "GET") {
    return json({ status: "ok", service: "shuttle-gateway" });
  }

  // Stateless Streamable HTTP MCP endpoint.
  if (path === "/mcp") {
    const principal = await authenticate(request, env, db);
    if (request.method === "GET") {
      return json({ status: "ok" });
    }
    if (request.method === "POST") {
      const body = (await readJson(request)) as unknown as RpcRequest;
      if (body.method === "notifications/initialized") {
        return new Response(null, { status: 202, headers: CORS_HEADERS });
      }
      const result = await handleMcp(body, { db, principal });
      return json(result);
    }
    return json({ error: "method not allowed" }, 405);
  }

  if (path.startsWith("/api/")) {
    const principal = await authenticate(request, env, db);
    const segments = path
      .slice("/api/".length)
      .split("/")
      .filter((segment) => segment.length > 0);
    return handleApi(request, db, principal, segments);
  }

  return json({ error: "not found" }, 404);
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    return handle(request, env, new D1Database_(env.DB));
  },
};
