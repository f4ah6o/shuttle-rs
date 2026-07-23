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
  claimTaskService,
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
  agentId: "test-agent",
  clientInstanceId: "test-client",
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

  it("keeps legacy local task ids addressable after migration", async () => {
    await createProjectService(db, admin, { slug: "alpha" });
    await appendEventService(db, await writable("alpha"), {
      event_id: "legacy-task-id",
      event_type: "task",
      agent: "codex",
      session_id: "legacy-session",
      title: "task",
      content: "migrate the old task",
      tags: ["task_open"],
      metadata: { action: "created", status: "open" },
    });

    const beforeClaim = await listTasksService(db, await readable("alpha"));
    expect(beforeClaim).toMatchObject([
      { task_id: "legacy-task-id", title: "migrate the old task", status: "open" },
    ]);

    await appendEventService(db, await writable("alpha"), {
      event_id: "legacy-claim-id",
      event_type: "task",
      agent: "old-codex",
      session_id: "legacy-claim-session",
      title: "task claim",
      content: "claimed task legacy-task-id",
      tags: ["task:claimed", "task_ref:legacy-task-id"],
      metadata: {
        action: "claimed",
        status: "claimed",
        task_id: "legacy-task-id",
        claimed_by: "old-codex",
      },
    });
    expect((await listTasksService(db, await readable("alpha")))[0]).toMatchObject({
      status: "claimed",
      claimed_by: "old-codex",
    });
    await expect(
      claimTaskService(db, await writable("alpha"), "legacy-task-id", {
        session_id: "competing-session",
      }),
    ).rejects.toThrow(/claim conflict/);

    await claimTaskService(db, await writable("alpha"), "legacy-task-id", {
      session_id: "takeover-session",
      takeover: true,
      reason: "old agent is no longer running",
    });
    await completeTaskService(db, await writable("alpha"), "legacy-task-id");

    const afterDone = await listTasksService(db, await readable("alpha"));
    expect(afterDone[0]).toMatchObject({ task_id: "legacy-task-id", status: "done" });
  });

  it("atomically claims tasks, is idempotent for the same agent, and rejects a competitor", async () => {
    await createProjectService(db, admin, { slug: "alpha" });
    const writableProject = await writable("alpha");
    const { task_id } = await createTaskService(db, writableProject, { title: "claim me" });

    const first = await claimTaskService(db, writableProject, task_id, {
      session_id: "first-session",
    });
    expect(first.deduplicated).toBe(false);
    expect(first.event.agent).toBe("test-agent");

    const retry = await claimTaskService(db, writableProject, task_id, {
      session_id: "retry-session",
    });
    expect(retry.deduplicated).toBe(true);
    expect(retry.event.id).toBe(first.event.id);

    const other: Principal = {
      ...principal,
      agentId: "other-agent",
    };
    const otherProject = await authorize(db, other, "alpha", "write");
    await expect(
      claimTaskService(db, otherProject, task_id, { session_id: "other-session" }),
    ).rejects.toThrow(/already claimed|claim conflict/);

    const takeover = await claimTaskService(db, otherProject, task_id, {
      session_id: "takeover-session",
      takeover: true,
      reason: "original agent stopped responding",
    });
    expect(takeover.takeover).toBe(true);
    expect(takeover.event.agent).toBe("other-agent");
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
    const reader: Principal = {
      ownerId: OWNER,
      scopes: new Set(["read"]),
      projectId: null,
      agentId: "reader",
      clientInstanceId: null,
    };
    await expect(authorize(db, reader, "alpha", "write")).rejects.toThrow(/write/);
  });
});
