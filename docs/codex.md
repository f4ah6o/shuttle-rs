# Codex

Codex-compatible agents should use [AGENTS.md](../AGENTS.md) as the canonical
project instructions file.

## Before Work

Start each session by reading Shuttle context and coordination state:

```bash
stl context
stl inbox
stl recall "current task"
stl task list
```

Prefer JSON output when passing data between tools:

```bash
stl --json context
stl --json inbox
stl --json recall "current task"
stl --json task list
```

If `SHUTTLE_AGENT` is not set, configure a repo-local identity once:

```bash
stl identity set codex
```

## During Work

Capture decisions and progress:

```bash
stl remember "important project note"
stl observe "what changed"
stl decide "important implementation decision"
stl task update <task-id> "Progress update"
```

Use messages for transient agent-to-agent communication:

```bash
stl send claude "Please review the latest diff"
stl inbox --watch
stl history
```

Use handoffs when another agent should continue:

```bash
stl handoff request claude "Please continue this branch"
stl handoff request opencode "Please review the latest diff"
```

Promote important message outcomes into durable state instead of leaving them
only in history:

```bash
stl decide --from-message <message-id>
stl task create --from-message <message-id>
stl handoff request claude --from-message <message-id>
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

For web chat clients that require a public remote MCP URL, run Shuttle through a
Cloudflare Named Tunnel:

```bash
SHUTTLE_OAUTH_ADMIN_TOKEN=<admin-token> \
CLOUDFLARE_TUNNEL_TOKEN=<cloudflare-tunnel-token> \
stl app tunnel --public-url https://shuttle.example.com
```

Register `https://shuttle.example.com/mcp` as the remote MCP endpoint. Keep the
Cloudflare token in a secret manager or runtime-injected environment variable.

If MCP is unavailable, use the CLI workflow directly.
