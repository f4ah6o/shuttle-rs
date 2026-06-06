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
root. The `.shuttle` directory is local runtime state and should not be
committed.

## Before Starting Work

Run these commands before making changes so your agent sees the current project
state and coordination queue:

```bash
stl context
stl recall "current task"
stl task list
```

Use JSON output when another tool or MCP client needs structured data:

```bash
stl --json context
stl --json recall "current task"
stl --json task list
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
stl history
```

## MCP

Start the Shuttle MCP server over stdio:

```bash
stl mcp serve
```

Use this command as the server command in MCP-compatible coding agents. The MCP
server exposes memory, recall, messages, context, task, handoff, and repository
tools with stable machine-readable responses.

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
