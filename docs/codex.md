# Codex

Codex-compatible agents should use [AGENTS.md](../AGENTS.md) as the canonical
project instructions file.

## Before Work

Start each session by reading Shuttle context and coordination state:

```bash
stl context
stl recall "current task"
stl task list
```

Prefer JSON output when passing data between tools:

```bash
stl --json context
stl --json recall "current task"
stl --json task list
```

## During Work

Capture decisions and progress:

```bash
stl remember "important project note"
stl observe "what changed"
stl decide "important implementation decision"
stl task update <task-id> "Progress update"
```

Use handoffs when another agent should continue:

```bash
stl handoff request claude "Please continue this branch"
stl handoff request opencode "Please review the latest diff"
```

## MCP

If the Codex environment supports MCP server configuration, register Shuttle
with:

```bash
stl mcp serve
```

Run the MCP server from the repository root so context, recall, task, handoff,
message, and repo tools use the same local `.shuttle/shuttle.db`.

If MCP is unavailable, use the CLI workflow directly.
