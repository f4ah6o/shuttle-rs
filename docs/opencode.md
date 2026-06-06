# opencode

Use Shuttle from opencode either as an MCP server or through the `stl` CLI.

## MCP Workflow

Build and initialize Shuttle from the repository you want to coordinate:

```bash
cargo build
cargo run -p stl -- init
```

Start Shuttle's HTTP MCP server from the repository root:

```bash
stl app serve --addr 127.0.0.1:8787
```

Register Shuttle as an MCP server with this endpoint:

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
`Authorization: Bearer <token>` on MCP requests. Use the repository root as the
working directory for the MCP server. That lets Shuttle attach repository
metadata and store events in `.shuttle/shuttle.db`.

For remote MCP clients such as web chat connectors, expose the same server
through a Cloudflare Named Tunnel:

```bash
SHUTTLE_OAUTH_ADMIN_TOKEN=<admin-token> \
CLOUDFLARE_TUNNEL_TOKEN=<cloudflare-tunnel-token> \
stl app tunnel --public-url https://shuttle.example.com
```

Register `https://shuttle.example.com/mcp` with the remote client.

Recommended startup workflow:

```text
Call Shuttle context, recall "current task", and list tasks before changing
files. Record decisions, observations, bugs, and handoffs in Shuttle as work
progresses.
```

## CLI Fallback

If MCP is unavailable, run the same workflow directly:

```bash
stl context
stl recall "current task"
stl task list
stl remember "important project note"
stl decide "important implementation decision"
stl handoff request codex "Please continue this branch"
```

Use `stl --json ...` for structured output that opencode can parse or summarize.
