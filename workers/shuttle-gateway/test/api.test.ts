import { beforeEach, describe, expect, it } from "vitest";

import type { Env } from "../src/env.js";
import { handle } from "../src/index.js";
import { makeRequest, NodeSqliteDatabase } from "./helpers.js";

const ADMIN = "admin-secret";
const env = { ADMIN_BOOTSTRAP_TOKEN: ADMIN, ADMIN_OWNER_ID: "owner-test" } as unknown as Env;

describe("resource API", () => {
  let db: NodeSqliteDatabase;

  beforeEach(() => {
    db = new NodeSqliteDatabase();
  });

  const call = (method: string, path: string, options: { token?: string; body?: unknown } = {}) =>
    handle(makeRequest(method, path, options), env, db);

  it("serves health without authentication", async () => {
    const response = await call("GET", "/api/health");
    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({ status: "ok" });
  });

  it("rejects unauthenticated requests", async () => {
    const response = await call("GET", "/api/projects");
    expect(response.status).toBe(401);
  });

  it("creates a project and appends an event idempotently", async () => {
    const created = await call("POST", "/api/projects", {
      token: ADMIN,
      body: { slug: "alpha", display_name: "Alpha" },
    });
    expect(created.status).toBe(201);

    const append = await call("POST", "/api/projects/alpha/events", {
      token: ADMIN,
      body: {
        event_id: "evt-1",
        event_type: "memory",
        agent: "codex",
        session_id: "s",
        content: "hello cloud",
        context: { repo: { branch: "main" } },
      },
    });
    expect(append.status).toBe(201);
    expect(await append.json()).toMatchObject({ deduplicated: false });

    const replay = await call("POST", "/api/projects/alpha/events", {
      token: ADMIN,
      body: { event_id: "evt-1", event_type: "memory", agent: "codex", session_id: "s", content: "hello cloud" },
    });
    expect(replay.status).toBe(200);
    expect(await replay.json()).toMatchObject({ deduplicated: true });
  });

  it("mints a project-scoped token that cannot reach other projects", async () => {
    await call("POST", "/api/projects", { token: ADMIN, body: { slug: "alpha" } });
    await call("POST", "/api/projects", { token: ADMIN, body: { slug: "beta" } });

    const mint = await call("POST", "/api/tokens", {
      token: ADMIN,
      body: { project: "alpha", scopes: ["read", "write"] },
    });
    expect(mint.status).toBe(201);
    const { token } = (await mint.json()) as { token: string };

    const allowed = await call("POST", "/api/projects/alpha/recall", {
      token,
      body: { query: "anything" },
    });
    expect(allowed.status).toBe(200);

    const denied = await call("POST", "/api/projects/beta/recall", {
      token,
      body: { query: "anything" },
    });
    expect(denied.status).toBe(403);

    const cannotCreate = await call("POST", "/api/projects", { token, body: { slug: "gamma" } });
    expect(cannotCreate.status).toBe(403);
  });

  it("disables the bootstrap token once an admin token is minted", async () => {
    // The bootstrap token works initially.
    const created = await call("POST", "/api/projects", { token: ADMIN, body: { slug: "alpha" } });
    expect(created.status).toBe(201);

    // Mint a persistent admin token using the bootstrap token.
    const mint = await call("POST", "/api/tokens", { token: ADMIN, body: { scopes: ["admin"] } });
    expect(mint.status).toBe(201);
    const { token: adminToken } = (await mint.json()) as { token: string };

    // The bootstrap token is now rejected — it is genuinely one-time.
    const afterBootstrap = await call("POST", "/api/projects", {
      token: ADMIN,
      body: { slug: "beta" },
    });
    expect(afterBootstrap.status).toBe(401);

    // The minted admin token continues to work.
    const withAdmin = await call("POST", "/api/projects", {
      token: adminToken,
      body: { slug: "beta" },
    });
    expect(withAdmin.status).toBe(201);
  });

  it("publishes and reads the latest context snapshot", async () => {
    await call("POST", "/api/projects", { token: ADMIN, body: { slug: "alpha" } });
    await call("POST", "/api/projects/alpha/context-snapshots", {
      token: ADMIN,
      body: { content: { branch: "main", dirty: false } },
    });
    const latest = await call("GET", "/api/projects/alpha/context-snapshots/latest", {
      token: ADMIN,
    });
    expect(latest.status).toBe(200);
    expect((await latest.json()) as { content: unknown }).toMatchObject({
      content: { branch: "main" },
    });
  });
});
