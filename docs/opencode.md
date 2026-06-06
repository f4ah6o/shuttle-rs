# opencode

Use Shuttle from opencode either as an MCP server or through the `stl` CLI.

## MCP Workflow

Build and initialize Shuttle from the repository you want to coordinate:

```bash
cargo build
cargo run -p stl -- init
```

Register Shuttle as an MCP server with this command:

```bash
stl mcp serve
```

Use the repository root as the working directory for the MCP server. That lets
Shuttle attach repository metadata and store events in `.shuttle/shuttle.db`.

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
