# Changes

## Unreleased

### Added

- Added repository-defined workflows with agent-independent run, step claim, checkpoint, takeover, reconciliation, and completion state.
- Added matching `stl workflow` commands and MCP tools so Claude Code and Codex can resume the same work.
- Added Shuttle skill installation for Claude Code alongside the existing Codex target.

### Changed

- `stl context` now includes active workflow runs and their next step.

### Fixed

### Deprecated

### Removed

### Security

### Migration

- Repositories opt in by adding a validated `shuttle.workflows.toml`; existing Shuttle databases and commands require no migration.
