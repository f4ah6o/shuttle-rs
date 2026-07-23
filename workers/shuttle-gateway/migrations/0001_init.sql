-- Cloud Shuttle gateway schema.
--
-- Projects are logical identities, not filesystem locations: a project is
-- usable with no repository path, local agent, or backend URL. Repository
-- metadata is stored verbatim from the client context envelope and is never
-- derived from a server-side filesystem.

CREATE TABLE owners (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL
);

CREATE TABLE projects (
  id TEXT PRIMARY KEY,
  owner_id TEXT NOT NULL,
  slug TEXT NOT NULL,
  display_name TEXT NOT NULL,
  description TEXT,
  canonical_git_remote TEXT,
  created_at TEXT NOT NULL,
  UNIQUE (owner_id, slug)
);

CREATE TABLE workspaces (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  client_instance_id TEXT NOT NULL,
  local_path_hint TEXT,
  created_at TEXT NOT NULL,
  FOREIGN KEY (project_id) REFERENCES projects(id)
);

-- Append-only shared event log. Canonical identity is (project_id, id): the
-- client-supplied event id is project-scoped, never globally unique. This keeps
-- replays/imports idempotent within a project while making cross-project
-- collisions impossible and future export/import/sharding straightforward.
CREATE TABLE events (
  id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  workspace_id TEXT,
  event_type TEXT NOT NULL,
  agent TEXT NOT NULL,
  session_id TEXT NOT NULL,
  title TEXT,
  content TEXT NOT NULL,
  git_remote TEXT,
  branch TEXT,
  commit_hash TEXT,
  repo_dirty INTEGER,
  metadata_json TEXT NOT NULL DEFAULT '{}',
  created_at TEXT NOT NULL,
  PRIMARY KEY (project_id, id),
  FOREIGN KEY (project_id) REFERENCES projects(id)
);

CREATE INDEX idx_events_project_created ON events(project_id, created_at);
CREATE INDEX idx_events_project_type_created ON events(project_id, event_type, created_at);

CREATE TABLE event_tags (
  project_id TEXT NOT NULL,
  event_id TEXT NOT NULL,
  tag TEXT NOT NULL,
  PRIMARY KEY (project_id, event_id, tag),
  FOREIGN KEY (project_id, event_id) REFERENCES events(project_id, id)
);

CREATE INDEX idx_event_tags_tag ON event_tags(tag);

-- Scoped personal access tokens for local coding agents. Tokens are stored as
-- SHA-256 hashes; the plaintext is shown once at mint time. A null project_id
-- grants the scope across every project owned by owner_id.
CREATE TABLE project_grants (
  token_hash TEXT PRIMARY KEY,
  owner_id TEXT NOT NULL,
  project_id TEXT,
  scopes TEXT NOT NULL,
  label TEXT,
  created_at TEXT NOT NULL
);

CREATE INDEX idx_grants_owner ON project_grants(owner_id);

-- Latest published repository/context snapshot per project. Large payloads can
-- later move to R2 and be referenced from here without changing semantics.
CREATE TABLE context_snapshots (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  workspace_id TEXT,
  agent TEXT,
  content_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  FOREIGN KEY (project_id) REFERENCES projects(id)
);

CREATE INDEX idx_snapshots_project_created ON context_snapshots(project_id, created_at);
