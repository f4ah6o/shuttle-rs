# Claude Code

Claude Code can use [CLAUDE.md](../CLAUDE.md) as its conventional entrypoint.
The canonical Shuttle workflow lives in [AGENTS.md](../AGENTS.md).

## Before Work

Run:

```bash
stl context
stl inbox
stl recall "current task"
stl task list
```

Use these results to identify active tasks, pending handoffs, recent decisions,
related memories, and repository status before editing files.

Set a repo-local identity when `SHUTTLE_AGENT` is not managed by the shell or
hook environment:

```bash
stl identity set claude
```

## During Work

Record useful events:

```bash
stl observe "what changed"
stl decide "important implementation decision"
stl bug "known issue or failing behavior"
stl task update <task-id> "Progress update"
```

Check messages at session start and stop. A human-visible terminal or Claude's
background Monitor can keep the non-terminating watcher running:

```bash
stl inbox --watch
```

Do not run `stl inbox --watch` directly inside a recurring prompt because it
does not return control. If Claude chooses Monitor, let the background monitor
own the watcher and do not schedule a second inbox poll.

## Recurring Collaboration

For explicitly requested unattended local collaboration, use Claude Code's
[`/loop`](https://code.claude.com/docs/en/scheduled-tasks) without a fixed
interval by default. Self-paced mode can lengthen quiet waits, use Monitor
instead of polling, and stop itself when work is complete. For example:

```text
/loop Use the shuttle skill to continue active collaboration work. Wait longer when the inbox is quiet, and stop the loop after completion, a blocker, an explicit stop, or three consecutive idle checks.
```

When no Monitor is active, start each loop run with the plain
`stl inbox --agent claude` output. If it has no actionable change and there is
no known active assignment, end the run without loading repository context,
running tests, spawning agents, or sending a no-op message. Request JSON only
when an actionable event ID is needed.

When work is available, load `stl context` and `stl task list`, tell the peer
which task and paths Claude intends to touch, complete and verify one bounded
unit, then send a concise result. Use `SHUTTLE_AGENT=claude` for CLI writes when
another client shares the checkout. Use a fixed interval only when the task
explicitly requires it because fixed loops cannot stop themselves before
expiry. In self-paced mode, call `ScheduleWakeup` with `stop: true` after a
terminal condition or three consecutive idle checks unless the task specifies
another budget.

Request or accept handoffs:

```bash
stl handoff request claude "Please continue this branch"
stl handoff list
stl handoff accept <handoff-id>
stl handoff done <handoff-id>
```

Promote message outcomes when they become durable decisions, bugs, tasks, or
handoffs:

```bash
stl decide --from-message <message-id>
stl bug --from-message <message-id>
stl task create --from-message <message-id>
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

For Claude web chat custom connectors, expose Shuttle through a Cloudflare
Named Tunnel and OAuth:

```bash
SHUTTLE_OAUTH_ADMIN_TOKEN=<admin-token> \
CLOUDFLARE_TUNNEL_TOKEN=<cloudflare-tunnel-token> \
stl app tunnel --public-url https://shuttle.example.com
```

Use `https://shuttle.example.com/mcp` as the remote MCP URL. The Cloudflare
token is read from the environment only; inject it at runtime rather than
printing or committing it.
