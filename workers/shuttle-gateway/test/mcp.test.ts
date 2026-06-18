import { beforeEach, describe, expect, it } from "vitest";

import type { Env } from "../src/env.js";
import { handle } from "../src/index.js";
import { makeRequest, NodeSqliteDatabase } from "./helpers.js";

const ADMIN = "admin-secret";
const env = { ADMIN_BOOTSTRAP_TOKEN: ADMIN, ADMIN_OWNER_ID: "owner-test" } as unknown as Env;

describe("MCP endpoint", () => {
  let db: NodeSqliteDatabase;

  beforeEach(() => {
    db = new NodeSqliteDatabase();
  });

  const rpc = (id: number, method: string, params?: unknown) =>
    handle(makeRequest("POST", "/mcp", { token: ADMIN, body: { jsonrpc: "2.0", id, method, params } }), env, db);

  it("lists tools", async () => {
    const response = await rpc(1, "tools/list");
    const body = (await response.json()) as { result: { tools: { name: string }[] } };
    const names = body.result.tools.map((tool) => tool.name);
    expect(names).toContain("shuttle_remember");
    expect(names).toContain("shuttle_recall");
    expect(names).toContain("shuttle_project_create");
  });

  it("requires authentication", async () => {
    const response = await handle(
      makeRequest("POST", "/mcp", { body: { jsonrpc: "2.0", id: 1, method: "tools/list" } }),
      env,
      db,
    );
    expect(response.status).toBe(401);
  });

  it("returns no content for the initialized notification", async () => {
    const response = await rpc(0, "notifications/initialized");
    expect(response.status).toBe(202);
  });

  it("creates a project, remembers, and recalls through tools/call", async () => {
    await rpc(1, "tools/call", {
      name: "shuttle_project_create",
      arguments: { slug: "alpha" },
    });

    const remember = await rpc(2, "tools/call", {
      name: "shuttle_remember",
      arguments: {
        project: "alpha",
        kind: "decision",
        text: "adopt cloudflare d1",
        context: { agent: "claude", session_id: "s1" },
      },
    });
    const rememberBody = (await remember.json()) as {
      result: { structuredContent: { deduplicated: boolean } };
    };
    expect(rememberBody.result.structuredContent.deduplicated).toBe(false);

    const recall = await rpc(3, "tools/call", {
      name: "shuttle_recall",
      arguments: { project: "alpha", query: "cloudflare" },
    });
    const recallBody = (await recall.json()) as {
      result: { structuredContent: { results: { event: { content: string } }[] } };
    };
    expect(recallBody.result.structuredContent.results[0].event.content).toBe(
      "adopt cloudflare d1",
    );
  });

  it("isolates projects so one client's selection cannot affect another", async () => {
    await rpc(1, "tools/call", { name: "shuttle_project_create", arguments: { slug: "alpha" } });
    await rpc(2, "tools/call", { name: "shuttle_project_create", arguments: { slug: "beta" } });

    await rpc(3, "tools/call", {
      name: "shuttle_remember",
      arguments: { project: "alpha", text: "alpha secret" },
    });

    const betaRecall = await rpc(4, "tools/call", {
      name: "shuttle_recall",
      arguments: { project: "beta", query: "alpha" },
    });
    const body = (await betaRecall.json()) as {
      result: { structuredContent: { results: unknown[] } };
    };
    expect(body.result.structuredContent.results).toHaveLength(0);
  });

  it("reports an error for an unknown tool", async () => {
    const response = await rpc(9, "tools/call", { name: "nope", arguments: {} });
    const body = (await response.json()) as { result: { isError?: boolean }; error?: unknown };
    // tools/call errors are surfaced as JSON-RPC errors.
    expect((body as { error?: { message: string } }).error?.message).toContain("unknown tool");
  });
});
