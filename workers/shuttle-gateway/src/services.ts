import type { AuthorizedAccount, AuthorizedProject } from "./auth.js";
import type { Database } from "./database.js";
import { badRequest, notFound } from "./errors.js";
import { newId, normalizeSlug } from "./ids.js";
import {
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
  },
): Promise<AppendResult> {
  requireNonEmpty(input.content, "content is required");
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
  options: { eventType?: EventType; limit?: number } = {},
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
  return events.filter((event) => (event.metadata_json as { task_id?: string }).task_id === taskId);
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

export async function listTasksService(
  db: Database,
  authorized: AuthorizedProject,
): Promise<TaskSummary[]> {
  const events = await listEvents(db, authorized.project.id, { eventType: "task", limit: 500 });
  const byTask = new Map<string, Event[]>();
  for (const event of events) {
    const taskId = (event.metadata_json as { task_id?: string }).task_id;
    if (!taskId) continue;
    const list = byTask.get(taskId) ?? [];
    list.push(event);
    byTask.set(taskId, list);
  }
  const summaries: TaskSummary[] = [];
  for (const [taskId, taskEvents] of byTask) {
    const ordered = [...taskEvents].sort((a, b) => a.created_at.localeCompare(b.created_at));
    const createEvent =
      ordered.find((event) => (event.metadata_json as { op?: string }).op === "create") ??
      ordered[0];
    const done = ordered.some((event) => (event.metadata_json as { op?: string }).op === "done");
    summaries.push({
      task_id: taskId,
      title: createEvent.title ?? createEvent.content,
      status: done ? "done" : "open",
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
