import { beforeEach, describe, expect, it } from "vitest";

import {
  authorize,
  authorizeAccount,
  type AuthorizedProject,
  type Principal,
} from "../src/auth.js";
import type { Database } from "../src/database.js";
import {
  appendEventService,
  completeTaskService,
  createProjectService,
  createTaskService,
  latestSnapshotService,
  listTasksService,
  publishSnapshotService,
  recallService,
  rememberService,
} from "../src/services.js";
import { NodeSqliteDatabase } from "./helpers.js";

const OWNER = "owner-test";
const principal: Principal = {
  ownerId: OWNER,
  scopes: new Set(["read", "write", "admin"]),
  projectId: null,
};
const admin = authorizeAccount(principal, "admin");

describe("application services", () => {
  let db: Database;

  beforeEach(() => {
    db = new NodeSqliteDatabase();
  });

  // Obtain an authorized project the same way the transports do, so tests
  // exercise the real authorization path rather than fabricating capabilities.
  const writable = (slug: string): Promise<AuthorizedProject<"write">> =>
    authorize(db, principal, slug, "write");
  const readable = (slug: string): Promise<AuthorizedProject<"read">> =>
    authorize(db, principal, slug, "read");

  it("creates a project with no repository, db, or backend url", async () => {
    const project = await createProjectService(db, admin, { slug: "alpha" });
    expect(project.slug).toBe("alpha");
    expect(project.display_name).toBe("alpha");
    expect(project.id).toMatch(/[0-9a-f-]{36}/);

    const resolved = await readable("alpha");
    expect(resolved.project.id).toBe(project.id);
  });

  it("rejects duplicate slugs", async () => {
    await createProjectService(db, admin, { slug: "alpha" });
    await expect(createProjectService(db, admin, { slug: "alpha" })).rejects.toThrow(/already/);
  });

  it("stores client-supplied repo metadata verbatim and never derives it", async () => {
    await createProjectService(db, admin, { slug: "alpha" });
    const result = await rememberService(db, await writable("alpha"), {
      kind: "decision",
      text: "use sqlite",
      context: {
        workspace_id: "ws-1",
        agent: "codex",
        session_id: "sess-1",
        repo: { git_remote: "git@example.test:repo.git", branch: "main", commit: "abc", dirty: true },
      },
    });
    expect(result.event.event_type).toBe("decision");
    expect(result.event.git_remote).toBe("git@example.test:repo.git");
    expect(result.event.branch).toBe("main");
    expect(result.event.repo_dirty).toBe(true);
    expect(result.event.workspace_id).toBe("ws-1");
    expect(result.event.agent).toBe("codex");
  });

  it("deduplicates events by event id (retry-safe)", async () => {
    await createProjectService(db, admin, { slug: "alpha" });
    const alpha = await writable("alpha");
    const first = await appendEventService(db, alpha, {
      event_id: "fixed-id-1",
      event_type: "memory",
      agent: "codex",
      session_id: "s",
      content: "remember me",
    });
    expect(first.deduplicated).toBe(false);

    const replay = await appendEventService(db, alpha, {
      event_id: "fixed-id-1",
      event_type: "memory",
      agent: "codex",
      session_id: "s",
      content: "remember me",
    });
    expect(replay.deduplicated).toBe(true);
    expect(replay.event.id).toBe(first.event.id);
  });

  it("treats the same client event id in two projects as distinct events", async () => {
    await createProjectService(db, admin, { slug: "alpha" });
    await createProjectService(db, admin, { slug: "beta" });

    const inAlpha = await appendEventService(db, await writable("alpha"), {
      event_id: "shared-id",
      event_type: "memory",
      agent: "codex",
      session_id: "s",
      content: "alpha content",
    });
    const inBeta = await appendEventService(db, await writable("beta"), {
      event_id: "shared-id",
      event_type: "memory",
      agent: "codex",
      session_id: "s",
      content: "beta content",
    });

    // Same id, different projects: neither is a dedupe hit and content is
    // isolated — the dedupe lookup never crosses the project boundary.
    expect(inAlpha.deduplicated).toBe(false);
    expect(inBeta.deduplicated).toBe(false);
    expect(inAlpha.event.content).toBe("alpha content");
    expect(inBeta.event.content).toBe("beta content");

    const replayBeta = await appendEventService(db, await writable("beta"), {
      event_id: "shared-id",
      event_type: "memory",
      agent: "codex",
      session_id: "s",
      content: "beta content",
    });
    expect(replayBeta.deduplicated).toBe(true);
    expect(replayBeta.event.content).toBe("beta content");
  });

  it("does not leak memory between two projects", async () => {
    await createProjectService(db, admin, { slug: "alpha" });
    await createProjectService(db, admin, { slug: "beta" });

    await rememberService(db, await writable("alpha"), { kind: "fact", text: "alpha uses postgres" });
    await rememberService(db, await writable("beta"), { kind: "fact", text: "beta uses redis" });

    const alphaHits = await recallService(db, await readable("alpha"), "postgres");
    const betaHits = await recallService(db, await readable("beta"), "postgres");

    expect(alphaHits.map((hit) => hit.event.content)).toContain("alpha uses postgres");
    expect(betaHits).toHaveLength(0);
  });

  it("projects tasks from the event log", async () => {
    await createProjectService(db, admin, { slug: "alpha" });
    const { task_id } = await createTaskService(db, await writable("alpha"), {
      title: "ship gateway",
    });

    let tasks = await listTasksService(db, await readable("alpha"));
    expect(tasks).toHaveLength(1);
    expect(tasks[0]).toMatchObject({ task_id, title: "ship gateway", status: "open" });

    await completeTaskService(db, await writable("alpha"), task_id);
    tasks = await listTasksService(db, await readable("alpha"));
    expect(tasks[0].status).toBe("done");
  });

  it("publishes and reads the latest context snapshot", async () => {
    await createProjectService(db, admin, { slug: "alpha" });
    await publishSnapshotService(db, await writable("alpha"), {
      content: { branch: "main", note: "first" },
    });
    await publishSnapshotService(db, await writable("alpha"), {
      content: { branch: "main", note: "second" },
    });

    const latest = await latestSnapshotService(db, await readable("alpha"));
    expect((latest?.content as { note: string }).note).toBe("second");
  });

  it("denies write access to a read-only principal", async () => {
    await createProjectService(db, admin, { slug: "alpha" });
    const reader: Principal = { ownerId: OWNER, scopes: new Set(["read"]), projectId: null };
    await expect(authorize(db, reader, "alpha", "write")).rejects.toThrow(/write/);
  });
});
