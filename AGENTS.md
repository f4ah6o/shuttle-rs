# Agent Guide

This repository provides `stl`, a local-first event log CLI for agent memory,
repository context, task coordination, handoffs, messaging, mesh sync, and MCP
access.

## Setup

Build the workspace and initialize Shuttle storage in the current Git
repository:

```bash
cargo build
cargo run -p stl -- init
```

After installing or aliasing `stl`, the same initialization is:

```bash
stl init
```

Shuttle stores local data in `.shuttle/shuttle.db` at the current Git repository
root for repositories that explicitly use local mode. `taskforward` is the
cloud-first exception: after migration, its `.shuttle` directory is absent and
`stl` uses the Cloudflare Worker + D1 project configured by
`SHUTTLE_GATEWAY_URL` / `SHUTTLE_GATEWAY_PROJECT`.

## Before Starting Work

Run these commands before making changes so your agent sees the current project
state and coordination queue:

```bash
stl context
stl inbox
stl recall "current task"
stl task list
```

Use JSON output when another tool or MCP client needs structured data:

```bash
stl --json context
stl --json inbox
stl --json recall "current task"
stl --json task list
```

If `SHUTTLE_AGENT` is not set by the shell or agent runtime, set a repo-local
identity in local mode:

```bash
stl identity set codex
stl identity current
```

## During Work

Record useful context as it happens:

```bash
stl remember "important project note"
stl observe "what changed"
stl decide "important implementation decision"
stl pattern "repeatable workflow or design pattern"
stl fact "stable project fact"
stl bug "known issue or failing behavior"
```

Recall by query, optionally narrowing to a memory kind:

```bash
stl recall "SQLite decision"
stl recall "SQLite decision" --type decision
```

When commands run inside a Git repository, Shuttle attaches repository metadata
including repo path, remote, branch, commit, dirty status, and dirty file names.

## Task Coordination

Tasks are projected from append-only events, so agents can coordinate without a
separate task table:

```bash
stl task create "Implement feature"
stl task list
stl task claim <task-id>
stl task update <task-id> "Progress update"
stl task done <task-id>
```

Use `stl context` to see open tasks, claimed tasks, pending handoffs, recent
completed handoffs, recent decisions, related memories, recent messages, and
inbox entries together.

## Handoffs

Request, inspect, accept, and complete handoffs between agents:

```bash
stl handoff request claude "Please continue this branch"
stl handoff list
stl handoff accept <handoff-id>
stl handoff done <handoff-id>
```

Messages use the same local event store:

```bash
stl send codex "Please review the latest diff"
stl inbox
stl inbox --watch
stl history
```

Use `stl send` for transient communication, `stl handoff` for ownership
transfer, `stl task` for trackable work, and typed memory commands for durable
outcomes. Promote important messages instead of leaving them only in history:

```bash
stl decide --from-message <message-id>
stl task create --from-message <message-id>
stl handoff request claude --from-message <message-id>
```

## MCP

Start the Shuttle HTTP MCP server:

```bash
stl app serve --addr 127.0.0.1:8787
```

Configure MCP-compatible coding agents with the HTTP endpoint:

```json
{
  "mcpServers": {
    "shuttle": {
      "url": "http://127.0.0.1:8787/mcp"
    }
  }
}
```

Set `SHUTTLE_MCP_BEARER_TOKEN` before starting the app server to require
`Authorization: Bearer <token>` on MCP requests. When unset, local MCP remains
unauthenticated.

## Mesh Sync

Replicate local event logs through archive import/export or direct database
sync:

```bash
stl mesh export shuttle-events.json
stl mesh import shuttle-events.json
stl mesh sync /path/to/peer/.shuttle/shuttle.db
```

Mesh sync preserves stable event ids, skips duplicates, and keeps imported
events visible in the receiving workspace.

## Cloud Sync (Cloudflare gateway)

For local-mode repositories, share the local event log across machines through the cloud shuttle-gateway
(the Cloudflare Worker in `workers/shuttle-gateway/`). To stand up the gateway
itself, see [docs/deploy-cloudflare.md](./docs/deploy-cloudflare.md). Configure
once per repository, then push/pull:

```bash
export SHUTTLE_GATEWAY_TOKEN=stl_...   # scoped PAT minted by the gateway
stl sync init --url https://<gateway-host> --project my-project
stl sync push   # upload local events (idempotent by event id)
stl sync pull   # download gateway events into this workspace
stl sync        # both: push, then pull
```

`stl sync init` writes `.shuttle/remote.json` (URL, project, and optionally
the token env var name via `--token-env`); the token itself is never stored.
Flags override the saved settings, and `SHUTTLE_GATEWAY_URL` /
`SHUTTLE_GATEWAY_PROJECT` work as fallbacks. Like mesh sync, cloud sync
preserves event ids, keeps original timestamps, skips duplicates, and makes
pulled events visible in the receiving workspace.

For `taskforward`, this push/pull mode is only the one-time migration path.
Normal commands become cloud-first automatically when `.shuttle/shuttle.db` is
absent and the gateway environment is present. The cloud-first runtime sends
Cloudflare Access service-auth headers plus a project-scoped PAT, auto-reuses a
workspace by `SHUTTLE_CLIENT_INSTANCE_ID`, and fails closed instead of creating
a local fallback database.
