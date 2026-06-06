# Claude Code

Claude Code can use [CLAUDE.md](../CLAUDE.md) as its conventional entrypoint.
The canonical Shuttle workflow lives in [AGENTS.md](../AGENTS.md).

## Before Work

Run:

```bash
stl context
stl recall "current task"
stl task list
```

Use these results to identify active tasks, pending handoffs, recent decisions,
related memories, and repository status before editing files.

## During Work

Record useful events:

```bash
stl observe "what changed"
stl decide "important implementation decision"
stl bug "known issue or failing behavior"
stl task update <task-id> "Progress update"
```

Request or accept handoffs:

```bash
stl handoff request claude "Please continue this branch"
stl handoff list
stl handoff accept <handoff-id>
stl handoff done <handoff-id>
```

## MCP

Start Shuttle's HTTP MCP server from the repository root:

```bash
stl app serve --addr 127.0.0.1:8787
```

When Claude Code is configured with MCP servers, use Shuttle with:

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
`Authorization: Bearer <token>` on MCP requests. If MCP configuration is not
available, use the CLI fallback commands above.
