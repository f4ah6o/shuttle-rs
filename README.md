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

[日本語版](./README.ja.md)

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

Set or inspect the local agent identity:

```bash
stl identity current
stl identity set codex
```

`SHUTTLE_AGENT` overrides the repo-local identity in `.shuttle/agent`. When
neither is set, Shuttle uses `unknown`.

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

Send and receive agent messages:

```bash
stl send claude "Please review the latest diff"
stl send --from claude codex "LGTM after the last fix"
stl inbox
stl inbox --agent claude
stl inbox --watch
stl history
```

Messages are stored in the same append-only event log as memories, tasks,
handoffs, and repository context. Use messages for transient agent-to-agent
communication, tasks for trackable work, handoffs for ownership transfer, and
typed memories for durable outcomes.

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

Promote an important message into durable project state:

```bash
stl decide --from-message <message-id>
stl task create --from-message <message-id>
stl handoff request claude --from-message <message-id>
```

Expose Shuttle over HTTP MCP:

```bash
stl app serve --addr 127.0.0.1:8787
```

The app serves a small JSON dashboard at `/` and API endpoints for dashboard
state, inbox, tasks, memories, and repository context. It also exposes the MCP
endpoint at `/mcp`.

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

Pass `--public-url` to `stl app serve` when the app is already behind a public
HTTPS endpoint and should publish OAuth metadata:

```bash
SHUTTLE_OAUTH_ADMIN_TOKEN=<admin-token> \
stl app serve --addr 127.0.0.1:8787 --public-url https://shuttle.example.com
```

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

The MCP server provides memory, message, task, handoff, repository context,
repository status, changed-file, and diff tools. Tool aliases such as `remember`
and namespaced tools such as `shuttle_memory_store` call the same local event
log.

## Multi-project Gateway

For web chat clients that should use one MCP server across several local
repositories, run `shuttle-gateway`. The gateway is the MCP, auth, and project
routing boundary. Each project can execute locally with `stl --json ...`, or it
can point at a repo-local `stl app serve` process over HTTP.

The remote deployment model is:

```text
gateway host / LXC
└─ shuttle-gateway

project environment
└─ stl app serve
   └─ repo + .shuttle/shuttle.db
```

Create a project config from `examples/projects.example.toml`. Use
`backend = "http"` with `url` for remote project environments, or
`backend = "local"` with an absolute `repo` path for compatibility mode. If
`backend` is omitted and `repo` is present, the project is treated as local.

Run the gateway with the config:

```bash
shuttle-gateway serve --config projects.toml
```

Use `--stl /path/to/stl` to choose the CLI binary for local backends, and
`--timeout <seconds>` to set local subprocess and HTTP backend timeouts.
`--addr` can override a single-listener config, but a config with multiple
`[[listeners]]` owns its listener addresses.

Listeners separate ingress/auth policy from project backend type. A public Web
Chat listener should use OAuth, while private LAN/Tailscale/local listeners can
use bearer auth or loopback-only unauthenticated access:

```toml
[[listeners]]
name = "public"
addr = "127.0.0.1:8787"
auth = "oauth"
public_url = "https://shuttle.example.com"
oauth_admin_token_env = "SHUTTLE_OAUTH_ADMIN_TOKEN"

[[listeners]]
name = "private"
addr = "127.0.0.1:8788"
auth = "bearer"
bearer_token_env = "SHUTTLE_GATEWAY_TOKEN"
```

The gateway requires an explicit `project` argument for writes such as
`shuttle_remember` and `shuttle_task_create`. Reads may use the configured
default project.

For HTTP backends, start a repo-local app server in the project environment:

```bash
SHUTTLE_MCP_BEARER_TOKEN=<backend-token> \
stl app serve --addr 127.0.0.1:8787
```

Then configure the gateway project with the URL and backend token environment
variable:

```toml
[projects.main]
backend = "http"
url = "http://10.10.10.21:8787"
token_env = "SHUTTLE_MAIN_BACKEND_TOKEN"
```

Register the public listener URL with remote MCP clients such as ChatGPT or
Claude web connectors:

```json
{
  "mcpServers": {
    "shuttle-gateway": {
      "url": "https://shuttle.example.com/mcp"
    }
  }
}
```

Run OAuth listeners with the owner-approval token injected at runtime:

```bash
SHUTTLE_OAUTH_ADMIN_TOKEN=<admin-token> \
shuttle-gateway serve --config projects.toml
```

OAuth client registrations, authorization codes, and access tokens are stored in
gateway-local SQLite databases. Backend tokens and OAuth admin tokens should be
provided by a secret manager or runtime-injected environment variables.

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
