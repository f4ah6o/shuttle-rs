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

  it("stores a client-supplied created_at and rejects an invalid one", async () => {
    await call("POST", "/api/projects", { token: ADMIN, body: { slug: "alpha" } });

    const append = await call("POST", "/api/projects/alpha/events", {
      token: ADMIN,
      body: {
        event_id: "evt-hist",
        event_type: "memory",
        agent: "codex",
        session_id: "s",
        content: "imported history",
        created_at: "2024-01-02T03:04:05Z",
      },
    });
    expect(append.status).toBe(201);
    const appended = (await append.json()) as { event: { created_at: string } };
    expect(appended.event.created_at).toBe("2024-01-02T03:04:05Z");

    const listed = await call("GET", "/api/projects/alpha/events", { token: ADMIN });
    const { events } = (await listed.json()) as { events: Array<{ created_at: string }> };
    expect(events[0].created_at).toBe("2024-01-02T03:04:05Z");

    const invalid = await call("POST", "/api/projects/alpha/events", {
      token: ADMIN,
      body: {
        event_type: "memory",
        agent: "codex",
        session_id: "s",
        content: "bad time",
        created_at: "not-a-date",
      },
    });
    expect(invalid.status).toBe(400);
  });

  it("paginates events with the before cursor without gaps or duplicates", async () => {
    await call("POST", "/api/projects", { token: ADMIN, body: { slug: "alpha" } });

    // Three events share one timestamp to exercise the id tie-break.
    const total = 12;
    for (let i = 0; i < total; i++) {
      const stamp =
        i < 3 ? "2024-06-01T00:00:00Z" : `2024-06-01T00:00:${String(i).padStart(2, "0")}Z`;
      const append = await call("POST", "/api/projects/alpha/events", {
        token: ADMIN,
        body: {
          event_id: `evt-${String(i).padStart(3, "0")}`,
          event_type: "memory",
          agent: "codex",
          session_id: "s",
          content: `event ${i}`,
          created_at: stamp,
        },
      });
      expect(append.status).toBe(201);
    }

    const seen: string[] = [];
    let before: string | null = null;
    let pages = 0;
    for (;;) {
      const query = before ? `?limit=5&before=${encodeURIComponent(before)}` : "?limit=5";
      const response = await call("GET", `/api/projects/alpha/events${query}`, { token: ADMIN });
      expect(response.status).toBe(200);
      const page = (await response.json()) as {
        events: Array<{ id: string }>;
        has_more: boolean;
        next_before: string | null;
      };
      seen.push(...page.events.map((event) => event.id));
      pages += 1;
      if (!page.has_more || page.events.length === 0) break;
      before = page.next_before;
    }

    expect(pages).toBeGreaterThanOrEqual(3);
    expect(seen.length).toBe(total);
    expect(new Set(seen).size).toBe(total);

    const malformed = await call("GET", "/api/projects/alpha/events?before=no-separator", {
      token: ADMIN,
    });
    expect(malformed.status).toBe(400);
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
