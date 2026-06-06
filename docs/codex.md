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

Start Shuttle's HTTP MCP server from the repository root:

```bash
stl app serve --addr 127.0.0.1:8787
```

If the Codex environment supports MCP server configuration, register Shuttle
with:

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
`Authorization: Bearer <token>` on MCP requests. Run the MCP server from the
repository root so context, recall, task, handoff, message, and repo tools use
the same local `.shuttle/shuttle.db`.

If MCP is unavailable, use the CLI workflow directly.
