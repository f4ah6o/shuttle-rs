-- Cloud-first coordination state. The event log remains the audit trail;
-- these tables hold the small mutable projections needed for atomic claims.

ALTER TABLE project_grants ADD COLUMN agent_id TEXT NOT NULL DEFAULT 'unknown-agent';
ALTER TABLE project_grants ADD COLUMN client_instance_id TEXT;

CREATE UNIQUE INDEX idx_workspaces_project_client
  ON workspaces(project_id, client_instance_id);

CREATE TABLE task_claims (
  project_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  claimed_by TEXT,
  claim_event_id TEXT,
  takeover_reason TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (project_id, task_id),
  FOREIGN KEY (project_id) REFERENCES projects(id)
);

CREATE TABLE workflow_step_claims (
  project_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  step_id TEXT NOT NULL,
  claimed_by TEXT,
  claim_event_id TEXT,
  takeover_reason TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (project_id, run_id, step_id),
  FOREIGN KEY (project_id) REFERENCES projects(id)
);

CREATE INDEX idx_task_claims_project ON task_claims(project_id, updated_at);
CREATE INDEX idx_workflow_claims_project ON workflow_step_claims(project_id, updated_at);

-- Seed claim rows from already-imported history. This is deliberately
-- idempotent: migration can be applied after a partial history import.
INSERT OR IGNORE INTO task_claims
  (project_id, task_id, claimed_by, claim_event_id, created_at, updated_at)
SELECT project_id,
       json_extract(metadata_json, '$.task_id'),
       COALESCE(json_extract(metadata_json, '$.claimed_by'), agent),
       id,
       created_at,
       created_at
FROM events
WHERE event_type = 'task'
  AND json_extract(metadata_json, '$.action') = 'claimed'
  AND json_extract(metadata_json, '$.task_id') IS NOT NULL;

INSERT OR IGNORE INTO workflow_step_claims
  (project_id, run_id, step_id, claimed_by, claim_event_id, created_at, updated_at)
SELECT project_id,
       json_extract(metadata_json, '$.run_id'),
       json_extract(metadata_json, '$.step_id'),
       agent,
       id,
       created_at,
       created_at
FROM events
WHERE event_type = 'workflow'
  AND json_extract(metadata_json, '$.action') IN ('claimed', 'reclaimed', 'taken_over')
  AND json_extract(metadata_json, '$.run_id') IS NOT NULL
  AND json_extract(metadata_json, '$.step_id') IS NOT NULL;
