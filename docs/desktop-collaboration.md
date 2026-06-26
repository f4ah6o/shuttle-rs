# Desktop Collaboration

Use Shuttle when Codex Desktop and Claude Desktop should work from the same
local task queue. Both clients connect to one Shuttle MCP server, backed by the
same repository-local `.shuttle/shuttle.db`.

## Start the Local MCP Server

Run Shuttle from the repository root:

```bash
stl init
stl app serve --addr 127.0.0.1:8787
```

Register the same MCP endpoint in both desktop clients:

```json
{
  "mcpServers": {
    "shuttle": {
      "url": "http://127.0.0.1:8787/mcp"
    }
  }
}
```

If you require local MCP authentication, set `SHUTTLE_MCP_BEARER_TOKEN` before
starting `stl app serve` and configure the desktop clients to send
`Authorization: Bearer <token>`. Inject the token at runtime; do not print it or
store it in repository files.

## Start Shared Work

Create a shared task and notify the peer agent:

```bash
stl identity set codex
stl collab start "Implement the checkout flow" --agents codex,claude
```

In Claude Desktop, use the Shuttle MCP tool `shuttle_collab_status` to see the
same task queue. In Codex Desktop, use the same tool or run:

```bash
stl collab status
```

## Coordinate During Work

Use the collaboration commands for the common handoff patterns:

```bash
stl collab nudge claude "Please review the validation output"
stl collab pass claude <task-id> "Implementation is done; please review"
stl collab status
```

The matching MCP tools are:

- `shuttle_collab_start`
- `shuttle_collab_status`
- `shuttle_collab_nudge`
- `shuttle_collab_pass`

These tools write normal Shuttle task, message, and handoff events. Existing
commands such as `stl task list`, `stl inbox`, `stl history`, and
`stl handoff list` continue to work.

## Recommended Roles

Use one shared task when both agents need the same context. Let the active
agent claim or update the task, use `collab nudge` for short messages, and use
`collab pass` when ownership should move to the other agent. For parallel work,
create separate tasks and let each desktop claim a different task.
