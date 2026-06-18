import type { Database, Row, Statement } from "./database.js";
import { newId, nowIso } from "./ids.js";
import type {
  AppendResult,
  ContextSnapshot,
  Event,
  EventInput,
  EventType,
  Project,
  Workspace,
} from "./types.js";

const MEMORY_TYPES: EventType[] = [
  "memory",
  "decision",
  "observation",
  "pattern",
  "fact",
  "bug",
];

function rowToProject(row: Row): Project {
  return {
    id: String(row.id),
    owner_id: String(row.owner_id),
    slug: String(row.slug),
    display_name: String(row.display_name),
    description: (row.description as string | null) ?? null,
    canonical_git_remote: (row.canonical_git_remote as string | null) ?? null,
    created_at: String(row.created_at),
  };
}

function rowToEvent(row: Row, tags: string[]): Event {
  const dirty = row.repo_dirty as number | null;
  return {
    id: String(row.id),
    project_id: String(row.project_id),
    workspace_id: (row.workspace_id as string | null) ?? null,
    event_type: String(row.event_type) as EventType,
    agent: String(row.agent),
    session_id: String(row.session_id),
    title: (row.title as string | null) ?? null,
    content: String(row.content),
    git_remote: (row.git_remote as string | null) ?? null,
    branch: (row.branch as string | null) ?? null,
    commit_hash: (row.commit_hash as string | null) ?? null,
    repo_dirty: dirty === null || dirty === undefined ? null : dirty !== 0,
    metadata_json: JSON.parse(String(row.metadata_json ?? "{}")),
    tags,
    created_at: String(row.created_at),
  };
}

export async function ensureOwner(db: Database, ownerId: string): Promise<void> {
  await db.run("INSERT OR IGNORE INTO owners (id, created_at) VALUES (?, ?)", [ownerId, nowIso()]);
}

export async function createProject(
  db: Database,
  input: {
    owner_id: string;
    slug: string;
    display_name: string;
    description?: string | null;
    canonical_git_remote?: string | null;
  },
): Promise<Project> {
  const project: Project = {
    id: newId(),
    owner_id: input.owner_id,
    slug: input.slug,
    display_name: input.display_name,
    description: input.description ?? null,
    canonical_git_remote: input.canonical_git_remote ?? null,
    created_at: nowIso(),
  };
  await db.run(
    `INSERT INTO projects (id, owner_id, slug, display_name, description, canonical_git_remote, created_at)
     VALUES (?, ?, ?, ?, ?, ?, ?)`,
    [
      project.id,
      project.owner_id,
      project.slug,
      project.display_name,
      project.description,
      project.canonical_git_remote,
      project.created_at,
    ],
  );
  return project;
}

export async function listProjects(db: Database, ownerId: string): Promise<Project[]> {
  const rows = await db.query(
    "SELECT * FROM projects WHERE owner_id = ? ORDER BY slug",
    [ownerId],
  );
  return rows.map(rowToProject);
}

export async function findProject(
  db: Database,
  ownerId: string,
  selector: string,
): Promise<Project | null> {
  const row = await db.first(
    "SELECT * FROM projects WHERE owner_id = ? AND (id = ? OR slug = ?)",
    [ownerId, selector, selector],
  );
  return row ? rowToProject(row) : null;
}

export async function createWorkspace(
  db: Database,
  projectId: string,
  input: { client_instance_id: string; local_path_hint?: string | null },
): Promise<Workspace> {
  const workspace: Workspace = {
    id: newId(),
    project_id: projectId,
    client_instance_id: input.client_instance_id,
    local_path_hint: input.local_path_hint ?? null,
    created_at: nowIso(),
  };
  await db.run(
    `INSERT INTO workspaces (id, project_id, client_instance_id, local_path_hint, created_at)
     VALUES (?, ?, ?, ?, ?)`,
    [
      workspace.id,
      workspace.project_id,
      workspace.client_instance_id,
      workspace.local_path_hint,
      workspace.created_at,
    ],
  );
  return workspace;
}

async function loadTags(db: Database, projectId: string, eventId: string): Promise<string[]> {
  const rows = await db.query(
    "SELECT tag FROM event_tags WHERE project_id = ? AND event_id = ? ORDER BY tag",
    [projectId, eventId],
  );
  return rows.map((row) => String(row.tag));
}

async function getEvent(db: Database, projectId: string, eventId: string): Promise<Event | null> {
  const row = await db.first("SELECT * FROM events WHERE project_id = ? AND id = ?", [
    projectId,
    eventId,
  ]);
  if (!row) return null;
  return rowToEvent(row, await loadTags(db, projectId, eventId));
}

/**
 * Append an event and its tags atomically. Idempotent by event id: replaying a
 * known id stores nothing new and reports `deduplicated: true`.
 */
export async function appendEvent(
  db: Database,
  projectId: string,
  input: EventInput,
): Promise<AppendResult> {
  const id = (input.event_id && input.event_id.trim()) || newId();

  // Dedupe is scoped to the project: the same client event id in two different
  // projects is two distinct events, never a cross-project hit.
  const existing = await getEvent(db, projectId, id);
  if (existing) {
    return { event: existing, deduplicated: true };
  }

  const repo = input.context?.repo ?? null;
  const tags = Array.from(new Set((input.tags ?? []).map((tag) => tag.trim()).filter(Boolean)));
  const metadata = { ...(input.metadata ?? {}) };
  if (repo) {
    metadata.repo = {
      git_remote: repo.git_remote ?? null,
      branch: repo.branch ?? null,
      commit: repo.commit ?? null,
      dirty: repo.dirty ?? null,
      dirty_files: repo.dirty_files ?? [],
    };
  }

  const event: Event = {
    id,
    project_id: projectId,
    workspace_id: input.context?.workspace_id ?? null,
    event_type: input.event_type,
    agent: input.agent,
    session_id: input.session_id,
    title: input.title ?? null,
    content: input.content,
    git_remote: repo?.git_remote ?? null,
    branch: repo?.branch ?? null,
    commit_hash: repo?.commit ?? null,
    repo_dirty: repo?.dirty ?? null,
    metadata_json: metadata,
    tags,
    created_at: nowIso(),
  };

  const statements: Statement[] = [
    {
      sql: `INSERT OR IGNORE INTO events (
              id, project_id, workspace_id, event_type, agent, session_id, title, content,
              git_remote, branch, commit_hash, repo_dirty, metadata_json, created_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      params: [
        event.id,
        event.project_id,
        event.workspace_id,
        event.event_type,
        event.agent,
        event.session_id,
        event.title,
        event.content,
        event.git_remote,
        event.branch,
        event.commit_hash,
        event.repo_dirty === null ? null : event.repo_dirty ? 1 : 0,
        JSON.stringify(event.metadata_json),
        event.created_at,
      ],
    },
    ...tags.map((tag) => ({
      sql: "INSERT OR IGNORE INTO event_tags (project_id, event_id, tag) VALUES (?, ?, ?)",
      params: [event.project_id, event.id, tag],
    })),
  ];

  await db.batch(statements);
  return { event, deduplicated: false };
}

export interface ListEventsOptions {
  eventType?: EventType;
  eventTypes?: EventType[];
  workspaceId?: string;
  limit?: number;
}

export async function listEvents(
  db: Database,
  projectId: string,
  options: ListEventsOptions = {},
): Promise<Event[]> {
  const clauses = ["project_id = ?"];
  const params: unknown[] = [projectId];
  const types = options.eventTypes ?? (options.eventType ? [options.eventType] : undefined);
  if (types && types.length > 0) {
    clauses.push(`event_type IN (${types.map(() => "?").join(", ")})`);
    params.push(...types);
  }
  if (options.workspaceId) {
    clauses.push("workspace_id = ?");
    params.push(options.workspaceId);
  }
  const limit = Math.max(1, Math.min(options.limit ?? 50, 500));
  const rows = await db.query(
    `SELECT * FROM events WHERE ${clauses.join(" AND ")} ORDER BY created_at DESC, id DESC LIMIT ?`,
    [...params, limit],
  );
  const events: Event[] = [];
  for (const row of rows) {
    events.push(rowToEvent(row, await loadTags(db, projectId, String(row.id))));
  }
  return events;
}

export function memoryEventTypes(): EventType[] {
  return [...MEMORY_TYPES];
}

export async function publishSnapshot(
  db: Database,
  projectId: string,
  input: { workspace_id?: string | null; agent?: string | null; content: unknown },
): Promise<ContextSnapshot> {
  const snapshot: ContextSnapshot = {
    id: newId(),
    project_id: projectId,
    workspace_id: input.workspace_id ?? null,
    agent: input.agent ?? null,
    content: input.content,
    created_at: nowIso(),
  };
  await db.run(
    `INSERT INTO context_snapshots (id, project_id, workspace_id, agent, content_json, created_at)
     VALUES (?, ?, ?, ?, ?, ?)`,
    [
      snapshot.id,
      snapshot.project_id,
      snapshot.workspace_id,
      snapshot.agent,
      JSON.stringify(snapshot.content),
      snapshot.created_at,
    ],
  );
  return snapshot;
}

export async function latestSnapshot(
  db: Database,
  projectId: string,
): Promise<ContextSnapshot | null> {
  const row = await db.first(
    "SELECT * FROM context_snapshots WHERE project_id = ? ORDER BY created_at DESC, id DESC LIMIT 1",
    [projectId],
  );
  if (!row) return null;
  return {
    id: String(row.id),
    project_id: String(row.project_id),
    workspace_id: (row.workspace_id as string | null) ?? null,
    agent: (row.agent as string | null) ?? null,
    content: JSON.parse(String(row.content_json)),
    created_at: String(row.created_at),
  };
}
