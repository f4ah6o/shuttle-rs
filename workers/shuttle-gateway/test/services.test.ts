import { beforeEach, describe, expect, it } from "vitest";

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
  resolveProject,
} from "../src/services.js";
import { NodeSqliteDatabase } from "./helpers.js";

const OWNER = "owner-test";

describe("application services", () => {
  let db: Database;

  beforeEach(() => {
    db = new NodeSqliteDatabase();
  });

  it("creates a project with no repository, db, or backend url", async () => {
    const project = await createProjectService(db, OWNER, { slug: "alpha" });
    expect(project.slug).toBe("alpha");
    expect(project.display_name).toBe("alpha");
    expect(project.id).toMatch(/[0-9a-f-]{36}/);

    const resolved = await resolveProject(db, OWNER, "alpha");
    expect(resolved.id).toBe(project.id);
  });

  it("rejects duplicate slugs", async () => {
    await createProjectService(db, OWNER, { slug: "alpha" });
    await expect(createProjectService(db, OWNER, { slug: "alpha" })).rejects.toThrow(/already/);
  });

  it("stores client-supplied repo metadata verbatim and never derives it", async () => {
    const project = await createProjectService(db, OWNER, { slug: "alpha" });
    const result = await rememberService(db, project, {
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
    const project = await createProjectService(db, OWNER, { slug: "alpha" });
    const first = await appendEventService(db, project, {
      event_id: "fixed-id-1",
      event_type: "memory",
      agent: "codex",
      session_id: "s",
      content: "remember me",
    });
    expect(first.deduplicated).toBe(false);

    const replay = await appendEventService(db, project, {
      event_id: "fixed-id-1",
      event_type: "memory",
      agent: "codex",
      session_id: "s",
      content: "remember me",
    });
    expect(replay.deduplicated).toBe(true);
    expect(replay.event.id).toBe(first.event.id);
  });

  it("does not leak memory between two projects", async () => {
    const alpha = await createProjectService(db, OWNER, { slug: "alpha" });
    const beta = await createProjectService(db, OWNER, { slug: "beta" });

    await rememberService(db, alpha, { kind: "fact", text: "alpha uses postgres" });
    await rememberService(db, beta, { kind: "fact", text: "beta uses redis" });

    const alphaHits = await recallService(db, alpha, "postgres");
    const betaHits = await recallService(db, beta, "postgres");

    expect(alphaHits.map((hit) => hit.event.content)).toContain("alpha uses postgres");
    expect(betaHits).toHaveLength(0);
  });

  it("projects tasks from the event log", async () => {
    const project = await createProjectService(db, OWNER, { slug: "alpha" });
    const { task_id } = await createTaskService(db, project, { title: "ship gateway" });

    let tasks = await listTasksService(db, project);
    expect(tasks).toHaveLength(1);
    expect(tasks[0]).toMatchObject({ task_id, title: "ship gateway", status: "open" });

    await completeTaskService(db, project, task_id);
    tasks = await listTasksService(db, project);
    expect(tasks[0].status).toBe("done");
  });

  it("publishes and reads the latest context snapshot", async () => {
    const project = await createProjectService(db, OWNER, { slug: "alpha" });
    await publishSnapshotService(db, project, { content: { branch: "main", note: "first" } });
    await publishSnapshotService(db, project, { content: { branch: "main", note: "second" } });

    const latest = await latestSnapshotService(db, project);
    expect((latest?.content as { note: string }).note).toBe("second");
  });
});
