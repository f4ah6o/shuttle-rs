# Changes

## Unreleased

### Added

- Added the OAuth `refresh_token` grant to `stl serve --public-url` and to gateway listeners with `auth = "oauth"`, so an MCP client renews its access token without a second owner approval. Exchanging an authorization code now also returns a refresh token, and the authorization-server metadata advertises `refresh_token` in `grant_types_supported`.

### Changed

- `stl skill install codex` and `stl skill install claude` now add client-specific, token-bounded recurring collaboration guidance for Codex scheduled tasks and Claude Code `/loop`.
- `POST /oauth/token` accepts a request without `redirect_uri`, which the refresh grant does not send. A missing `redirect_uri` in an authorization-code request is now reported as an OAuth `invalid_grant` response instead of a form-extraction failure.
- `POST /oauth/revoke` revokes every access token that belongs to the same authorization when it is given a refresh token. An access token still revokes only itself.

### Fixed

### Deprecated

### Removed

### Security

- Refresh tokens rotate on every use and are stored as digests. A consumed refresh token presented more than 30 seconds after its consumption is treated as theft and revokes every refresh token and access token derived from that authorization, with one warn-level log line carrying only `client_id` and `family_id`.

### Migration

- The OAuth database schema moves from version 3 to version 4 on first open, adding `oauth_refresh_tokens` and a `family_id` column on `oauth_tokens`. The upgrade runs automatically for `stl serve` and for each gateway listener database. Access tokens issued before the upgrade keep working until they expire, but they belong to no authorization family, so revoking a refresh token does not reach them.

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
