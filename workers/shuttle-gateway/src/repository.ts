import type { Database, Row, Statement } from "./database.js";
import { newId, nowIso } from "./ids.js";
import type {
  AppendResult,
  ClaimResult,
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
  const existing = await db.first(
    "SELECT * FROM workspaces WHERE project_id = ? AND client_instance_id = ?",
    [projectId, input.client_instance_id],
  );
  if (existing) {
    if (input.local_path_hint && input.local_path_hint !== existing.local_path_hint) {
      await db.run("UPDATE workspaces SET local_path_hint = ? WHERE id = ?", [
        input.local_path_hint,
        existing.id,
      ]);
    }
    const refreshed = await db.first("SELECT * FROM workspaces WHERE id = ?", [existing.id]);
    return rowToWorkspace(refreshed ?? existing);
  }

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

function rowToWorkspace(row: Row): Workspace {
  return {
    id: String(row.id),
    project_id: String(row.project_id),
    client_instance_id: String(row.client_instance_id),
    local_path_hint: (row.local_path_hint as string | null) ?? null,
    created_at: String(row.created_at),
  };
}

async function loadTags(db: Database, projectId: string, eventId: string): Promise<string[]> {
  const rows = await db.query(
    "SELECT tag FROM event_tags WHERE project_id = ? AND event_id = ? ORDER BY tag",
    [projectId, eventId],
  );
  return rows.map((row) => String(row.tag));
}

export async function getEvent(db: Database, projectId: string, eventId: string): Promise<Event | null> {
  const row = await db.first("SELECT * FROM events WHERE project_id = ? AND id = ?", [
    projectId,
    eventId,
  ]);
  if (!row) return null;
  return rowToEvent(row, await loadTags(db, projectId, eventId));
}

export interface AtomicClaimInput {
  table: "task_claims" | "workflow_step_claims";
  key: Record<string, string>;
  event: Event;
  claimed_by: string;
  takeover: boolean;
  takeover_reason?: string | null;
}

/**
 * Claim a task/workflow step and append its audit event in one D1 batch.
 *
 * The event id doubles as a private claim operation id. A competing request
 * can therefore never append an audit event merely because another request
 * already owns the row: the conditional event INSERT only matches the claim
 * operation that actually changed the row.
 */
export async function appendAtomicClaim(
  db: Database,
  input: AtomicClaimInput,
): Promise<ClaimResult> {
  const keyColumns = Object.keys(input.key);
  if (keyColumns.length === 0) throw new Error("atomic claim requires a key");
  const keyValues = keyColumns.map((column) => input.key[column]);
  const keyWhere = keyColumns.map((column) => `${column} = ?`).join(" AND ");
  const keyWhereWithProject = `project_id = ? AND ${keyWhere}`;
  const table = input.table;
  const now = input.event.created_at;

  const columns = ["project_id", ...keyColumns, "claimed_by", "claim_event_id", "takeover_reason", "created_at", "updated_at"];
  const placeholders = columns.map(() => "?").join(", ");
  const insertParams = [
    input.event.project_id,
    ...keyValues,
    null,
    null,
    null,
    now,
    now,
  ];
  const updateSet = [
    "claimed_by = ?",
    "claim_event_id = ?",
    "takeover_reason = ?",
    "updated_at = ?",
  ].join(", ");
  const updateParams = [
    input.claimed_by,
    input.event.id,
    input.takeover_reason ?? null,
    now,
    input.event.project_id,
    ...keyValues,
    input.takeover ? 1 : 0,
    input.claimed_by,
  ];

  const event = input.event;
  const statements: Statement[] = [
    {
      sql: `INSERT OR IGNORE INTO ${table} (${columns.join(", ")}) VALUES (${placeholders})`,
      params: insertParams,
    },
    {
      sql: `UPDATE ${table} SET ${updateSet}
            WHERE ${keyWhereWithProject}
              AND (claimed_by IS NULL OR (? = 1 AND claimed_by <> ?))`,
      params: updateParams,
    },
    {
      sql: `INSERT OR IGNORE INTO events (
              id, project_id, workspace_id, event_type, agent, session_id, title, content,
              git_remote, branch, commit_hash, repo_dirty, metadata_json, created_at
            )
            SELECT ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?
            WHERE EXISTS (
              SELECT 1 FROM ${table}
              WHERE ${keyWhereWithProject} AND claim_event_id = ? AND claimed_by = ?
            )`,
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
        event.project_id,
        ...keyValues,
        event.id,
        input.claimed_by,
      ],
    },
    ...event.tags.map((tag) => ({
      sql: `INSERT OR IGNORE INTO event_tags (project_id, event_id, tag)
            SELECT ?, ?, ?
            WHERE EXISTS (SELECT 1 FROM ${table}
                          WHERE ${keyWhereWithProject} AND claim_event_id = ? AND claimed_by = ?)`,
      params: [
        event.project_id,
        event.id,
        tag,
        event.project_id,
        ...keyValues,
        event.id,
        input.claimed_by,
      ],
    })),
  ];
  await db.batch(statements);

  const claim = await db.first(
    `SELECT claimed_by, claim_event_id FROM ${table} WHERE ${keyWhereWithProject}`,
    [event.project_id, ...keyValues],
  );
  const claimedBy = (claim?.claimed_by as string | null) ?? null;
  const claimEventId = (claim?.claim_event_id as string | null) ?? null;
  if (claimEventId === event.id) {
    const stored = await getEvent(db, event.project_id, event.id);
    if (!stored) throw new Error("claim succeeded without an audit event");
    return { event: stored, deduplicated: false, takeover: input.takeover };
  }
  if (claimedBy === input.claimed_by && claimEventId) {
    const stored = await getEvent(db, event.project_id, claimEventId);
    if (stored) return { event: stored, deduplicated: true, takeover: false };
  }
  throw new Error(`claim conflict: already claimed by ${claimedBy ?? "another agent"}`);
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
    created_at: input.created_at ?? nowIso(),
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

  // Migration pushes use the ordinary idempotent event endpoint rather than
  // the live atomic-claim endpoint. Keep the mutable claim projections in
  // step with those historical claim events too, so applying migrations before
  // importing a frozen local log does not erase ownership.
  const action = metadata.action;
  if (event.event_type === "task" && action === "claimed") {
    const taskId = metadata.task_id;
    if (typeof taskId === "string" && taskId.trim()) {
      await db.run(
        `INSERT INTO task_claims
           (project_id, task_id, claimed_by, claim_event_id, takeover_reason, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(project_id, task_id) DO UPDATE SET
           claimed_by = excluded.claimed_by,
           claim_event_id = excluded.claim_event_id,
           takeover_reason = excluded.takeover_reason,
           updated_at = excluded.updated_at`,
        [
          event.project_id,
          taskId,
          typeof metadata.claimed_by === "string" ? metadata.claimed_by : event.agent,
          event.id,
          typeof metadata.takeover_reason === "string" ? metadata.takeover_reason : null,
          event.created_at,
          event.created_at,
        ],
      );
    }
  }
  if (event.event_type === "workflow" && ["claimed", "reclaimed", "taken_over"].includes(String(action))) {
    const runId = metadata.run_id;
    const stepId = metadata.step_id;
    if (typeof runId === "string" && runId.trim() && typeof stepId === "string" && stepId.trim()) {
      await db.run(
        `INSERT INTO workflow_step_claims
           (project_id, run_id, step_id, claimed_by, claim_event_id, takeover_reason, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(project_id, run_id, step_id) DO UPDATE SET
           claimed_by = excluded.claimed_by,
           claim_event_id = excluded.claim_event_id,
           takeover_reason = excluded.takeover_reason,
           updated_at = excluded.updated_at`,
        [
          event.project_id,
          runId,
          stepId,
          event.agent,
          event.id,
          typeof metadata.takeover_reason === "string" ? metadata.takeover_reason : null,
          event.created_at,
          event.created_at,
        ],
      );
    }
  }
  return { event, deduplicated: false };
}

export interface ListEventsOptions {
  eventType?: EventType;
  eventTypes?: EventType[];
  workspaceId?: string;
  limit?: number;
  /** Keyset cursor: return only events strictly older than this position. */
  before?: { createdAt: string; id: string };
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
  if (options.before) {
    clauses.push("(created_at < ? OR (created_at = ? AND id < ?))");
    params.push(options.before.createdAt, options.before.createdAt, options.before.id);
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
    "SELECT * FROM context_snapshots WHERE project_id = ? ORDER BY created_at DESC, rowid DESC LIMIT 1",
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
