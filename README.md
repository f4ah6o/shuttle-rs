# shuttle-rs
<!-- bdg:begin -->
[![crates.io](https://img.shields.io/crates/v/shuttle-rs.svg)](https://crates.io/crates/shuttle-rs)
[![crates.io downloads](https://img.shields.io/crates/d/shuttle-rs.svg)](https://crates.io/crates/shuttle-rs)
[![docs.rs](https://docs.rs/shuttle-rs/badge.svg)](https://docs.rs/shuttle-rs)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](https://github.com/shuttle-rs/shuttle-rs)
[![release](https://img.shields.io/github/v/release/shuttle-rs/shuttle-rs.svg)](https://github.com/shuttle-rs/shuttle-rs/releases)
[![codecov](https://img.shields.io/codecov/c/github/shuttle-rs/shuttle-rs.svg)](https://codecov.io/gh/shuttle-rs/shuttle-rs)
[![CI](https://github.com/shuttle-rs/shuttle-rs/actions/workflows/publish.yaml/badge.svg)](https://github.com/shuttle-rs/shuttle-rs/actions/workflows/publish.yaml)
<!-- bdg:end -->

`shuttle-rs` is a local-first event log for agent memory, messaging, repository
context, and coordination. The `stl` CLI stores data in `.shuttle/shuttle.db`
under the current Git repository.

## Agent Onboarding

Use [AGENTS.md](./AGENTS.md) as the canonical workflow guide for coding agents.
Tool-specific setup paths are available for [opencode](./docs/opencode.md),
[Claude Code](./docs/claude-code.md), and [Codex](./docs/codex.md). Claude Code
can also use [CLAUDE.md](./CLAUDE.md) as its conventional entrypoint.

Install the bundled Codex skill for Shuttle:

```bash
stl skill install codex
```

Preview the generated skill without writing it:

```bash
stl skill print codex
```

## Phase 1 Commands

Initialize local storage:

```bash
stl init
```

Capture generic and typed memories:

```bash
stl remember "SQLite is the local event store"
stl decide "SQLite remains the default because it is local-first"
stl observe "The current branch changes recall ranking"
stl pattern "Use event projections rather than separate state tables"
stl fact "The event log is append-only"
stl bug "Recall ordering should prefer same-repo decisions"
```

Recall entries with ranked results:

```bash
stl recall "SQLite decision"
stl recall "SQLite decision" --type decision
stl --json recall "SQLite decision"
```

Read repository context:

```bash
stl context
stl context --repo
stl context --branch
stl --json context
```

Coordinate tasks and handoffs:

```bash
stl task create "Implement stl context"
stl task list
stl task claim <task-id>
stl task update <task-id> "Narrowed the implementation"
stl task done <task-id>

stl handoff request claude "Please continue this branch"
stl handoff list
stl handoff accept <handoff-id>
stl handoff done <handoff-id>
```

Expose Shuttle over HTTP MCP:

```bash
stl app serve --addr 127.0.0.1:8787
```

Configure MCP clients to use the `/mcp` endpoint:

```json
{
  "mcpServers": {
    "shuttle": {
      "url": "http://127.0.0.1:8787/mcp"
    }
  }
}
```

Set `SHUTTLE_MCP_BEARER_TOKEN` before starting `stl app serve` to require
`Authorization: Bearer <token>` on MCP requests. When the variable is unset,
local MCP remains unauthenticated.

Expose Shuttle as a remote MCP server for web chat clients with a Cloudflare
Named Tunnel:

```bash
SHUTTLE_OAUTH_ADMIN_TOKEN=<admin-token> \
CLOUDFLARE_TUNNEL_TOKEN=<cloudflare-tunnel-token> \
stl app tunnel --public-url https://shuttle.example.com
```

Configure ChatGPT or Claude with `https://shuttle.example.com/mcp`. The tunnel
token is read only from the environment, so use a secret manager or runtime
injection instead of putting it in shell history. The public URL must match the
Cloudflare Tunnel hostname that forwards to `http://127.0.0.1:8787`.

## Multi-project Gateway

For web chat clients that should use one MCP server across several local
repositories, run `shuttle-gateway`. The gateway keeps Shuttle storage
project-local by routing each request to a configured repository and running the
shared `stl --json ...` executable as a subprocess in that repository.

Create a project config from `examples/projects.example.toml`, then run:

```bash
shuttle-gateway serve --config projects.toml --addr 127.0.0.1:8787
```

Register one MCP endpoint:

```json
{
  "mcpServers": {
    "shuttle-gateway": {
      "url": "http://127.0.0.1:8787/mcp"
    }
  }
}
```

The gateway requires an explicit `project` argument for writes such as
`shuttle_remember` and `shuttle_task_create`. Reads may use the configured
default project. Set `SHUTTLE_GATEWAY_TOKEN` to require
`Authorization: Bearer <token>` at the gateway boundary.

To expose the gateway to remote MCP clients such as ChatGPT or Claude web
connectors, configure OAuth with the public URL that forwards to the gateway:

```toml
[oauth]
public_url = "https://shuttle.example.com"
admin_token_env = "SHUTTLE_OAUTH_ADMIN_TOKEN"
```

Then run the gateway with the owner-approval token injected at runtime:

```bash
SHUTTLE_OAUTH_ADMIN_TOKEN=<admin-token> \
shuttle-gateway serve --config projects.toml --addr 127.0.0.1:8787
```

Register `https://shuttle.example.com/mcp` with the remote MCP client. OAuth
client registrations, authorization codes, and access tokens are stored in a
gateway-local SQLite database, defaulting to `gateway-oauth.db` next to the
config file unless `[oauth].db_path` is set to an absolute path. Keep the admin
token in a secret manager or runtime-injected environment variable.

Synchronize event logs between Shuttle instances:

```bash
stl mesh export shuttle-events.json
stl mesh import shuttle-events.json
stl mesh sync /path/to/peer/.shuttle/shuttle.db
```

When commands run inside a Git repository, Shuttle attaches repository metadata
to captured events: repository path, remote-derived repository id when present,
branch, commit, dirty status, and dirty file names.

Task and handoff state is projected from append-only events. No separate task
table is required, and JSON output remains suitable for MCP clients.

Mesh synchronization imports events by stable event id and skips duplicates, so
re-running a sync after an offline period only transfers events that the other
store has not seen yet. The CLI normalizes imported events to the receiving
workspace id and records the source workspace in event metadata, which keeps
synced tasks, handoffs, messages, and memories visible to local commands.

## Acknowledgements

Shuttle is inspired by [kioku-mesh](https://github.com/h-wata/kioku-mesh), a
shared memory system for AI coding agents across tools and machines.

Shuttle also builds on ideas from [rally-rs](https://github.com/f4ah6o/rally-rs),
which in turn builds on [agmsg](https://github.com/fujibee/agmsg).
