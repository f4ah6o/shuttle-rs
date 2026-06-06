# shuttle-rs

`shuttle-rs` is a local-first event log for agent memory, messaging, repository
context, and coordination. The `stl` CLI stores data in `.shuttle/shuttle.db`
under the current Git repository.

## Agent Onboarding

Use [AGENTS.md](./AGENTS.md) as the canonical workflow guide for coding agents.
Tool-specific setup paths are available for [opencode](./docs/opencode.md),
[Claude Code](./docs/claude-code.md), and [Codex](./docs/codex.md). Claude Code
can also use [CLAUDE.md](./CLAUDE.md) as its conventional entrypoint.

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

Expose Shuttle over MCP:

```bash
stl mcp serve
stl app serve --addr 127.0.0.1:8787
```

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
