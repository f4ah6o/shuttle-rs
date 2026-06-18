export interface Project {
  id: string;
  owner_id: string;
  slug: string;
  display_name: string;
  description: string | null;
  canonical_git_remote: string | null;
  created_at: string;
}

export interface Workspace {
  id: string;
  project_id: string;
  client_instance_id: string;
  local_path_hint: string | null;
  created_at: string;
}

/**
 * Repository metadata captured by a local agent. The gateway stores it
 * verbatim and never derives it from a server-side filesystem.
 */
export interface RepoContext {
  git_remote?: string | null;
  branch?: string | null;
  commit?: string | null;
  dirty?: boolean | null;
  dirty_files?: string[];
}

/** Explicit context envelope sent by clients when storing events. */
export interface ContextEnvelope {
  workspace_id?: string | null;
  agent?: string | null;
  session_id?: string | null;
  repo?: RepoContext | null;
}

export const EVENT_TYPES = [
  "message",
  "memory",
  "decision",
  "task",
  "handoff",
  "observation",
  "pattern",
  "fact",
  "bug",
  "artifact",
] as const;

export type EventType = (typeof EVENT_TYPES)[number];

export interface EventInput {
  /** Optional client-supplied id. Replaying the same id is idempotent. */
  event_id?: string | null;
  event_type: EventType;
  agent: string;
  session_id: string;
  title?: string | null;
  content: string;
  tags?: string[];
  context?: ContextEnvelope | null;
  metadata?: Record<string, unknown> | null;
}

export interface Event {
  id: string;
  project_id: string;
  workspace_id: string | null;
  event_type: EventType;
  agent: string;
  session_id: string;
  title: string | null;
  content: string;
  git_remote: string | null;
  branch: string | null;
  commit_hash: string | null;
  repo_dirty: boolean | null;
  metadata_json: Record<string, unknown>;
  tags: string[];
  created_at: string;
}

export interface AppendResult {
  event: Event;
  /** True when an event with this id already existed. */
  deduplicated: boolean;
}

export interface RecallResult {
  event: Event;
  score: number;
}

export interface TaskSummary {
  task_id: string;
  title: string;
  status: "open" | "done";
  created_at: string;
  updated_at: string;
}

export interface ContextSnapshot {
  id: string;
  project_id: string;
  workspace_id: string | null;
  agent: string | null;
  content: unknown;
  created_at: string;
}
