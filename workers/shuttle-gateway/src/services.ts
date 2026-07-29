import type { AuthorizedAccount, AuthorizedProject } from "./auth.js";
import type { Database } from "./database.js";
import { badRequest, conflict, notFound } from "./errors.js";
import { newId, normalizeSlug } from "./ids.js";
import {
  appendAtomicClaim,
  appendEvent,
  createProject,
  createWorkspace,
  findProject,
  latestSnapshot,
  listEvents,
  listProjects,
  memoryEventTypes,
  publishSnapshot,
} from "./repository.js";
import type {
  AppendResult,
  ClaimResult,
  ContextEnvelope,
  ContextSnapshot,
  Event,
  EventType,
  Project,
  RecallResult,
  TaskSummary,
  Workspace,
} from "./types.js";
import { EVENT_TYPES } from "./types.js";

const MEMORY_KIND_TO_TYPE: Record<string, EventType> = {
  "": "memory",
  memory: "memory",
  decision: "decision",
  observation: "observation",
  pattern: "pattern",
  fact: "fact",
  bug: "bug",
};

function requireNonEmpty(value: string | undefined | null, message: string): string {
  const trimmed = (value ?? "").trim();
  if (!trimmed) throw badRequest(message);
  return trimmed;
}

function validatedCreatedAt(value?: string | null): string {
  if (value && value.trim()) {
    const trimmed = value.trim();
    if (Number.isNaN(Date.parse(trimmed))) {
      throw badRequest(`invalid created_at ${JSON.stringify(trimmed)}`);
    }
    return trimmed;
  }
  return new Date().toISOString();
}

function claimEvent(
  projectId: string,
  agent: string,
  input: {
    event_id?: string | null;
    event_type: EventType;
    session_id?: string | null;
    title: string;
    content: string;
    context?: ContextEnvelope | null;
    metadata: Record<string, unknown>;
    tags: string[];
    created_at?: string | null;
  },
) {
  const repo = input.context?.repo ?? null;
  const metadata: Record<string, unknown> = { ...input.metadata };
  if (repo) {
    metadata.repo = {
      git_remote: repo.git_remote ?? null,
      branch: repo.branch ?? null,
      commit: repo.commit ?? null,
      dirty: repo.dirty ?? null,
      dirty_files: repo.dirty_files ?? [],
    };
  }
  return {
    id: (input.event_id && input.event_id.trim()) || newId(),
    project_id: projectId,
    workspace_id: input.context?.workspace_id ?? null,
    event_type: input.event_type,
    agent,
    session_id: (input.session_id ?? "").trim() || newId(),
    title: input.title,
    content: input.content,
    git_remote: repo?.git_remote ?? null,
    branch: repo?.branch ?? null,
    commit_hash: repo?.commit ?? null,
    repo_dirty: repo?.dirty ?? null,
    metadata_json: metadata,
    tags: Array.from(new Set(input.tags.map((tag) => tag.trim()).filter(Boolean))),
    created_at: validatedCreatedAt(input.created_at),
  };
}

export async function createProjectService(
  db: Database,
  account: AuthorizedAccount<"admin">,
  input: {
    slug: string;
    display_name?: string | null;
    description?: string | null;
    canonical_git_remote?: string | null;
  },
): Promise<Project> {
  const ownerId = account.principal.ownerId;
  const slug = normalizeSlug(requireNonEmpty(input.slug, "slug is required"));
  if (await findProject(db, ownerId, slug)) {
    throw badRequest(`project ${JSON.stringify(slug)} already exists`);
  }
  return createProject(db, {
    owner_id: ownerId,
    slug,
    display_name: (input.display_name ?? "").trim() || slug,
    description: input.description ?? null,
    canonical_git_remote: input.canonical_git_remote ?? null,
  });
}

export function listProjectsService(db: Database, account: AuthorizedAccount): Promise<Project[]> {
  return listProjects(db, account.principal.ownerId);
}

export function createWorkspaceService(
  db: Database,
  authorized: AuthorizedProject<"write">,
  input: { client_instance_id: string; local_path_hint?: string | null },
): Promise<Workspace> {
  requireNonEmpty(input.client_instance_id, "client_instance_id is required");
  return createWorkspace(db, authorized.project.id, input);
}

function normalizeEventType(value: string): EventType {
  if ((EVENT_TYPES as readonly string[]).includes(value)) {
    return value as EventType;
  }
  throw badRequest(`unknown event_type ${JSON.stringify(value)}`);
}

export function appendEventService(
  db: Database,
  authorized: AuthorizedProject<"write">,
  input: {
    event_id?: string | null;
    event_type: string;
    agent: string;
    session_id: string;
    title?: string | null;
    content: string;
    tags?: string[];
    context?: ContextEnvelope | null;
    metadata?: Record<string, unknown> | null;
    created_at?: string | null;
  },
): Promise<AppendResult> {
  requireNonEmpty(input.content, "content is required");
  let createdAt: string | null = null;
  if (input.created_at != null && input.created_at.trim()) {
    const trimmed = input.created_at.trim();
    if (Number.isNaN(Date.parse(trimmed))) {
      throw badRequest(`invalid created_at ${JSON.stringify(trimmed)}`);
    }
    createdAt = trimmed;
  }
  return appendEvent(db, authorized.project.id, {
    event_id: input.event_id ?? null,
    event_type: normalizeEventType(input.event_type),
    agent: (input.agent ?? "").trim() || "unknown",
    session_id: (input.session_id ?? "").trim() || newId(),
    title: input.title ?? null,
    content: input.content,
    tags: input.tags ?? [],
    context: input.context ?? null,
    metadata: input.metadata ?? null,
    created_at: createdAt,
  });
}

export function rememberService(
  db: Database,
  authorized: AuthorizedProject<"write">,
  input: { kind?: string | null; text: string; context?: ContextEnvelope | null },
): Promise<AppendResult> {
  const kind = (input.kind ?? "").trim();
  const eventType = MEMORY_KIND_TO_TYPE[kind];
  if (!eventType) throw badRequest(`unknown memory kind ${JSON.stringify(kind)}`);
  return appendEventService(db, authorized, {
    event_type: eventType,
    agent: input.context?.agent ?? "unknown",
    session_id: input.context?.session_id ?? "",
    content: requireNonEmpty(input.text, "text is required"),
    context: input.context ?? null,
    metadata: { kind: eventType },
  });
}

export function listEventsService(
  db: Database,
  authorized: AuthorizedProject,
  options: {
    eventType?: EventType;
    id?: string;
    workspaceId?: string;
    agent?: string;
    recipient?: string;
    tag?: string;
    query?: string;
    after?: { createdAt: string; id: string };
    limit?: number;
    before?: { createdAt: string; id: string };
  } = {},
): Promise<Event[]> {
  return listEvents(db, authorized.project.id, options);
}

function tokenize(query: string): string[] {
  return query
    .toLowerCase()
    .split(/[^a-z0-9]+/)
    .filter((token) => token.length > 1);
}

export async function recallService(
  db: Database,
  authorized: AuthorizedProject,
  query: string,
  limit = 20,
): Promise<RecallResult[]> {
  requireNonEmpty(query, "query is required");
  const events = await listEvents(db, authorized.project.id, {
    eventTypes: memoryEventTypes(),
    limit: 200,
  });
  const tokens = tokenize(query);
  const results = events.map((event) => {
    const haystack = `${event.title ?? ""} ${event.content} ${event.tags.join(" ")}`.toLowerCase();
    let score = 0;
    for (const token of tokens) {
      if (haystack.includes(token)) score += 1;
    }
    return { event, score };
  });
  results.sort(
    (left, right) =>
      right.score - left.score || right.event.created_at.localeCompare(left.event.created_at),
  );
  return results.filter((result) => result.score > 0).slice(0, limit);
}

export async function createTaskService(
  db: Database,
  authorized: AuthorizedProject<"write">,
  input: { title: string; body?: string | null; context?: ContextEnvelope | null },
): Promise<{ task_id: string; result: AppendResult }> {
  const title = requireNonEmpty(input.title, "title is required");
  const body = (input.body ?? "").trim();
  const content = body ? `${title}\n\n${body}` : title;
  const taskId = newId();
  const result = await appendEventService(db, authorized, {
    event_type: "task",
    agent: input.context?.agent ?? "unknown",
    session_id: input.context?.session_id ?? "",
    title,
    content,
    context: input.context ?? null,
    metadata: { task_id: taskId, op: "create" },
  });
  return { task_id: taskId, result };
}

async function findTaskEvents(
  db: Database,
  authorized: AuthorizedProject,
  taskId: string,
): Promise<Event[]> {
  const events = await listEvents(db, authorized.project.id, { eventType: "task", limit: 500 });
  return events.filter((event) => taskIdForEvent(event) === taskId);
}

interface TaskMetadata {
  action?: string;
  op?: string;
  task_id?: string;
}

function taskMetadata(event: Event): TaskMetadata {
  return event.metadata_json as TaskMetadata;
}

/**
 * The first local Shuttle task event used its own event id as the task id and
 * only the follow-up events carried `metadata.task_id`. Keep that history
 * addressable after migration while using explicit ids for new events.
 */
function taskIdForEvent(event: Event): string | null {
  const metadata = taskMetadata(event);
  if (typeof metadata.task_id === "string" && metadata.task_id.trim()) {
    return metadata.task_id;
  }
  const reference = event.tags.find((tag) => tag.startsWith("task_ref:"));
  if (reference) return reference.slice("task_ref:".length);
  if (metadata.action === "created" || event.tags.includes("task_open") || event.tags.includes("task:open")) {
    return event.id;
  }
  return null;
}

function isTaskCreated(event: Event): boolean {
  const metadata = taskMetadata(event);
  return (
    metadata.op === "create" ||
    metadata.action === "created" ||
    event.tags.includes("task_open") ||
    event.tags.includes("task:open")
  );
}

function isTaskDone(event: Event): boolean {
  const metadata = taskMetadata(event);
  return (
    metadata.op === "done" ||
    metadata.action === "completed" ||
    event.tags.includes("task_done") ||
    event.tags.includes("task:done")
  );
}

function isClaimConflict(error: unknown): error is Error {
  return error instanceof Error && error.message.startsWith("claim conflict:");
}

export async function updateTaskService(
  db: Database,
  authorized: AuthorizedProject<"write">,
  taskId: string,
  text: string,
  context?: ContextEnvelope | null,
): Promise<AppendResult> {
  requireNonEmpty(taskId, "task_id is required");
  const text_ = requireNonEmpty(text, "text is required");
  if ((await findTaskEvents(db, authorized, taskId)).length === 0) {
    throw notFound(`unknown task ${JSON.stringify(taskId)}`);
  }
  return appendEventService(db, authorized, {
    event_type: "task",
    agent: context?.agent ?? "unknown",
    session_id: context?.session_id ?? "",
    content: text_,
    context: context ?? null,
    metadata: { task_id: taskId, op: "update" },
  });
}

export async function completeTaskService(
  db: Database,
  authorized: AuthorizedProject<"write">,
  taskId: string,
  context?: ContextEnvelope | null,
): Promise<AppendResult> {
  requireNonEmpty(taskId, "task_id is required");
  if ((await findTaskEvents(db, authorized, taskId)).length === 0) {
    throw notFound(`unknown task ${JSON.stringify(taskId)}`);
  }
  return appendEventService(db, authorized, {
    event_type: "task",
    agent: context?.agent ?? "unknown",
    session_id: context?.session_id ?? "",
    content: `task ${taskId} done`,
    context: context ?? null,
    metadata: { task_id: taskId, op: "done" },
  });
}

export async function claimTaskService(
  db: Database,
  authorized: AuthorizedProject<"write">,
  taskId: string,
  input: {
    event_id?: string | null;
    session_id?: string | null;
    context?: ContextEnvelope | null;
    created_at?: string | null;
    takeover?: boolean;
    reason?: string | null;
  },
): Promise<ClaimResult> {
  const taskEvents = await findTaskEvents(db, authorized, taskId);
  if (taskEvents.length === 0) throw notFound(`unknown task ${JSON.stringify(taskId)}`);
  const ordered = [...taskEvents].sort((left, right) =>
    left.created_at.localeCompare(right.created_at) || left.id.localeCompare(right.id),
  );
  const latest = ordered[ordered.length - 1];
  if (isTaskDone(latest)) {
    throw badRequest(`task ${taskId} is already done`);
  }

  const takeover = input.takeover === true;
  const reason = input.reason?.trim() || null;
  if (takeover && !reason) throw badRequest("takeover reason is required");
  const agent = authorized.principal.agentId;
  const event = claimEvent(authorized.project.id, agent, {
    event_id: input.event_id,
    event_type: "task",
    session_id: input.session_id,
    title: "task claim",
    content: `${takeover ? "took over" : "claimed"} task ${taskId}`,
    context: input.context,
    metadata: {
      action: "claimed",
      status: "claimed",
      task_id: taskId,
      claimed_by: agent,
      ...(takeover ? { takeover: true, takeover_reason: reason } : {}),
    },
    tags: ["task_claimed", `claim:${agent}`, `task_ref:${taskId}`],
    created_at: input.created_at,
  });
  try {
    return await appendAtomicClaim(db, {
      table: "task_claims",
      key: { task_id: taskId },
      event,
      claimed_by: agent,
      takeover,
      takeover_reason: reason,
    });
  } catch (error) {
    if (isClaimConflict(error)) throw conflict(error.message);
    throw error;
  }
}

interface WorkflowStepState {
  id: string;
  kind: string;
  status: "pending" | "claimed" | "completed" | "failed" | "needs_reconcile";
  claimed_by: string | null;
}

async function workflowStepStates(
  db: Database,
  projectId: string,
  runId: string,
): Promise<{ runStatus: string; steps: WorkflowStepState[] }> {
  const events = await db.query(
    "SELECT * FROM events WHERE project_id = ? AND event_type = 'workflow' AND json_extract(metadata_json, '$.run_id') = ? ORDER BY created_at ASC, id ASC",
    [projectId, runId],
  );
  const started = events.find(
    (event) => JSON.parse(String(event.metadata_json)).action === "started",
  );
  if (!started) throw notFound(`workflow run ${JSON.stringify(runId)} not found`);
  const startedMetadata = JSON.parse(String(started.metadata_json)) as {
    steps?: Array<{ id?: string; kind?: string }>;
  };
  const steps: WorkflowStepState[] = (startedMetadata.steps ?? []).map((step) => ({
    id: String(step.id ?? ""),
    kind: String(step.kind ?? "read_only"),
    status: "pending",
    claimed_by: null,
  }));
  let runStatus = "active";
  for (const event of events) {
    const metadata = JSON.parse(String(event.metadata_json)) as {
      action?: string;
      step_id?: string;
      value?: { output?: unknown };
    };
    if (metadata.action === "completed") runStatus = "completed";
    if (metadata.action === "aborted") runStatus = "aborted";
    const step = steps.find((candidate) => candidate.id === metadata.step_id);
    if (!step) continue;
    if (["claimed", "reclaimed", "taken_over"].includes(metadata.action ?? "")) {
      if (metadata.action === "taken_over" && step.kind === "non_idempotent") {
        step.status = "needs_reconcile";
      } else {
        step.status = "claimed";
      }
      step.claimed_by = String(event.agent);
    } else if (metadata.action === "completed_step" || metadata.action === "reconciled") {
      step.status = "completed";
      step.claimed_by = null;
    } else if (metadata.action === "failed_step") {
      step.status = "failed";
      step.claimed_by = String(event.agent);
      runStatus = "failed";
    }
  }
  return { runStatus, steps };
}

export async function claimWorkflowStepService(
  db: Database,
  authorized: AuthorizedProject<"write">,
  runId: string,
  stepId: string,
  input: {
    event_id?: string | null;
    session_id?: string | null;
    context?: ContextEnvelope | null;
    created_at?: string | null;
    takeover?: boolean;
    reason?: string | null;
  },
): Promise<ClaimResult> {
  const state = await workflowStepStates(db, authorized.project.id, runId);
  if (["completed", "aborted"].includes(state.runStatus)) {
    throw badRequest(`workflow run ${runId} is not active`);
  }
  const step = state.steps.find((candidate) => candidate.id === stepId);
  if (!step) throw notFound(`unknown workflow step ${JSON.stringify(stepId)}`);
  const expected = state.steps.find((candidate) => candidate.status !== "completed");
  if (!expected || expected.id !== stepId) {
    throw badRequest(`next workflow step is ${expected?.id ?? "none"}, not ${stepId}`);
  }
  const takeover = input.takeover === true;
  const reason = input.reason?.trim() || null;
  if (takeover && !reason) throw badRequest("takeover reason is required");
  if (step.status === "needs_reconcile") {
    throw conflict(`workflow step ${stepId} needs reconciliation`);
  }
  if (takeover && step.status !== "claimed") {
    throw badRequest(`workflow step ${stepId} is not claimed; claim it without takeover`);
  }
  if (takeover && step.kind === "non_idempotent") {
    throw conflict(`workflow step ${stepId} needs reconciliation before takeover`);
  }
  const action = step.status === "failed" ? "reclaimed" : takeover ? "taken_over" : "claimed";
  const agent = authorized.principal.agentId;
  const event = claimEvent(authorized.project.id, agent, {
    event_id: input.event_id,
    event_type: "workflow",
    session_id: input.session_id,
    title: "workflow step claim",
    content: `workflow ${runId}: ${action}`,
    context: input.context,
    metadata: {
      action,
      run_id: runId,
      step_id: stepId,
      ...(takeover ? { takeover: true, takeover_reason: reason } : {}),
    },
    tags: [`workflow_run:${runId}`, `workflow:${action}`],
    created_at: input.created_at,
  });
  try {
    return await appendAtomicClaim(db, {
      table: "workflow_step_claims",
      key: { run_id: runId, step_id: stepId },
      event,
      claimed_by: agent,
      takeover,
      takeover_reason: reason,
    });
  } catch (error) {
    if (isClaimConflict(error)) throw conflict(error.message);
    throw error;
  }
}

export async function listTasksService(
  db: Database,
  authorized: AuthorizedProject,
): Promise<TaskSummary[]> {
  const events = await listEvents(db, authorized.project.id, { eventType: "task", limit: 500 });
  const byTask = new Map<string, Event[]>();
  for (const event of events) {
    const taskId = taskIdForEvent(event);
    if (!taskId) continue;
    const list = byTask.get(taskId) ?? [];
    list.push(event);
    byTask.set(taskId, list);
  }
  const summaries: TaskSummary[] = [];
  for (const [taskId, taskEvents] of byTask) {
    const ordered = [...taskEvents].sort((a, b) => a.created_at.localeCompare(b.created_at));
    const createEvent = ordered.find(isTaskCreated) ?? ordered[0];
    const done = ordered.some(isTaskDone);
    const claim = await db.first(
      "SELECT claimed_by FROM task_claims WHERE project_id = ? AND task_id = ?",
      [authorized.project.id, taskId],
    );
    summaries.push({
      task_id: taskId,
      title: createEvent.title && createEvent.title !== "task" ? createEvent.title : createEvent.content,
      status: done ? "done" : claim?.claimed_by ? "claimed" : "open",
      claimed_by: (claim?.claimed_by as string | null) ?? null,
      created_at: createEvent.created_at,
      updated_at: ordered[ordered.length - 1].created_at,
    });
  }
  summaries.sort((a, b) => b.created_at.localeCompare(a.created_at));
  return summaries;
}

export function publishSnapshotService(
  db: Database,
  authorized: AuthorizedProject<"write">,
  input: { workspace_id?: string | null; agent?: string | null; content: unknown },
): Promise<ContextSnapshot> {
  if (input.content === undefined || input.content === null) {
    throw badRequest("content is required");
  }
  return publishSnapshot(db, authorized.project.id, input);
}

export function latestSnapshotService(
  db: Database,
  authorized: AuthorizedProject,
): Promise<ContextSnapshot | null> {
  return latestSnapshot(db, authorized.project.id);
}
