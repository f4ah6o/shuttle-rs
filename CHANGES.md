# Changes

## Unreleased

### Added

### Changed

### Fixed

### Deprecated

### Removed

### Security

### Migration

## 2026.7.0 - 2026-07-23

### Added

- Added repository-defined workflows with agent-independent run, step claim, checkpoint, takeover, reconciliation, and completion state.
- Added matching `stl workflow` commands and MCP tools so Claude Code and Codex can resume the same work.
- Added Shuttle skill installation for Claude Code alongside the existing Codex target.

### Changed

- `stl context` now includes active workflow runs and their next step.
- Reconciling an approval-required workflow step now requires the same approval evidence as completing it (`stl workflow reconcile --approval`, MCP `approval` argument).
- Claiming a step that is not currently claimed with `--takeover` is now rejected; takeover is only valid for claimed steps.

### Fixed

- Fixed failed workflow steps being impossible to retry: re-claiming a failed step no longer collides with the duplicate-claim protection.

### Deprecated

### Removed

### Security

### Migration

- Repositories opt in by adding a validated `shuttle.workflows.toml`; existing Shuttle databases and commands require no migration.
