# shuttle-rs

[日本語](./README.ja.md)

[![crates.io](https://img.shields.io/crates/v/shuttle-rs.svg)](https://crates.io/crates/shuttle-rs)
[![docs.rs](https://docs.rs/shuttle-rs/badge.svg)](https://docs.rs/shuttle-rs)
[![CI](https://github.com/f4ah6o/shuttle-rs/actions/workflows/ci.yaml/badge.svg)](https://github.com/f4ah6o/shuttle-rs/actions/workflows/ci.yaml)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](./LICENSE-MIT)

`shuttle-rs` gives coding agents a shared append-only event log for memory, messages, tasks, handoffs, and repository context.
In local mode, the `stl` CLI stores events in `.shuttle/shuttle.db` in the target Git repository.
When that database is absent and `SHUTTLE_GATEWAY_URL` is set, `stl` uses the Cloudflare Worker and D1 project selected by `SHUTTLE_GATEWAY_PROJECT`.
`shuttle-gateway` exposes multiple projects through one MCP endpoint.

## Capabilities

- **Memory**: Store memories, decisions, observations, patterns, facts, and bugs, then rank recall results with repository metadata.
- **Repository context**: Associate events with branch, commit, dirty state, and changed files.
- **Agent coordination**: Manage messages, tasks, handoffs, and a shared Codex and Claude Desktop collaboration queue in the same event log.
- **Workflows**: Read repository-owned `shuttle.workflows.toml` manifests and record runs, claims, checkpoints, and reconciliation.
- **MCP**: Serve a local HTTP endpoint, publish an OAuth-enabled endpoint, or route several projects through a gateway.
- **Synchronization**: Move events through JSON archives, SQLite databases, or a Cloudflare Worker backed by D1.
- **Adapter routing**: Build a deterministic repository embedding, select registered LoRA or PEFT adapters, and export a runtime manifest.
- **JSON output**: Add `--json` to return machine-readable results for agents and scripts.

## Installation

Install from crates.io with a stable Rust toolchain and Git.

```bash
cargo install shuttle-rs --locked
```

The package installs both `stl` and `shuttle-gateway`.

```bash
stl version
shuttle-gateway version
```

To run from source, clone the repository and use Cargo.

```bash
cargo build
cargo run --bin stl -- --help
cargo run --bin shuttle-gateway -- --help
```

## Local setup

Initialize storage at the root of a Git repository and set an agent identity.

```bash
stl init
stl identity set codex
```

Store a decision, create a task, and inspect the repository state.

```bash
stl decide "Use SQLite as the local event store"
stl task create "Verify MCP authentication"
stl context
```

`stl init` creates `.shuttle/shuttle.db` for local mode.
The append-only event log projects memories, messages, tasks, handoffs, and workflow state without separate mutable state tables.

## Cloud-first setup

Cloud-first mode uses the Cloudflare Worker and D1 as the authoritative event store.
It is selected when `.shuttle/shuttle.db` is absent and `SHUTTLE_GATEWAY_URL` is set.
Set the gateway URL, project, and project-scoped token before running ordinary `stl` commands.

```bash
export SHUTTLE_GATEWAY_URL=https://<gateway-host>
export SHUTTLE_GATEWAY_PROJECT=my-project
export SHUTTLE_GATEWAY_TOKEN=stl_...
stl context
```

`stl init` refuses to create a local database in cloud-first mode.
The token environment variable defaults to `SHUTTLE_GATEWAY_TOKEN` unless `.shuttle/remote.json` names another variable.

## Memory and repository context

Store information with a type that reflects its role.

```bash
stl remember "SQLite is the local event store"
stl decide "Use append-only events"
stl observe "The branch changed"
stl pattern "Project state is rebuilt from events"
stl fact "The database path is .shuttle/shuttle.db"
stl bug "Recall ranking needs inspection"
```

Recall by text and optional type.

```bash
stl recall "SQLite"
stl recall "SQLite" --type decision
stl --json recall "SQLite"
```

Read repository-wide or branch-scoped context.

```bash
stl context
stl context --repo
stl context --branch
stl --json context
```

## Messages, tasks, and handoffs

Use messages for short-lived agent communication.

```bash
stl send claude "Please review the latest diff"
stl inbox
stl inbox --agent claude
stl inbox --watch
stl history
```

Use tasks for work that needs ownership and progress tracking.

```bash
stl task create "Implement repository status"
stl task list
stl task claim <task-id>
stl task update <task-id> "Added tests"
stl task done <task-id>
```

Use handoffs when work moves to another agent.

```bash
stl handoff request claude "Please continue this branch"
stl handoff list
stl handoff accept <handoff-id>
stl handoff done <handoff-id>
```

Promote a message into durable project state.

```bash
stl decide --from-message <message-id>
stl task create --from-message <message-id>
stl handoff request claude --from-message <message-id>
```

Manage a shared queue for Codex Desktop and Claude Desktop.

```bash
stl collab start "Implement the checkout flow" --agents codex,claude
stl collab status
stl collab nudge claude "Please review the validation output"
stl collab pass claude <task-id> "Implementation is done; please review"
```

## Repository workflows

`shuttle.workflows.toml` defines workflows by pointing at specifications owned by the repository.
Each step is classified as `read_only`, `idempotent`, or `non_idempotent`.

```bash
stl workflow list
stl workflow show daily-triage
stl workflow start daily-triage
stl workflow step claim <run-id> read-spec
stl workflow step complete <run-id> read-spec --output '{"read":true}'
stl workflow status <run-id>
```

An interrupted step remains claimed.
When another agent assumes it, claim it with `--takeover` and record the reason.
For a claimed `non_idempotent` step, that transition changes the status to `needs_reconcile`.

```bash
stl workflow step claim <run-id> <step-id> \
  --takeover \
  --reason "Previous agent was interrupted"
```

Inspect the external system instead of repeating the operation, then record the observed result.

```bash
stl workflow reconcile <run-id> <step-id> --output '{"status":"completed"}'
```

## HTTP MCP server

Start an HTTP server for the target repository.

```bash
stl app serve --addr 127.0.0.1:8787
```

`/mcp` is the MCP endpoint.
`/` and `/api/*` return JSON for inbox entries, tasks, memories, and repository context.

Configure an MCP client with the endpoint.

```json
{
  "mcpServers": {
    "shuttle": {
      "url": "http://127.0.0.1:8787/mcp"
    }
  }
}
```

Set a token before startup to require Bearer authentication.

```bash
SHUTTLE_MCP_BEARER_TOKEN=<token> \
stl app serve --addr 127.0.0.1:8787
```

Set an owner approval token and a public URL to publish OAuth metadata.

```bash
SHUTTLE_OAUTH_ADMIN_TOKEN=<admin-token> \
stl app serve \
  --addr 127.0.0.1:8787 \
  --public-url https://shuttle.example.com
```

`stl` can also launch a Cloudflare Named Tunnel.

```bash
SHUTTLE_OAUTH_ADMIN_TOKEN=<admin-token> \
CLOUDFLARE_TUNNEL_TOKEN=<cloudflare-tunnel-token> \
stl app tunnel --public-url https://shuttle.example.com
```

## Multi-project gateway

`shuttle-gateway` combines authentication and project selection in one MCP server.
A project can use a `local` backend that opens a repository directly or an `http` backend that calls `stl app serve`.

See [`examples/projects.example.toml`](./examples/projects.example.toml) for a complete configuration.

```toml
[defaults]
project = "main"

[[listeners]]
name = "private"
addr = "127.0.0.1:8788"
auth = "bearer"
bearer_token_env = "SHUTTLE_GATEWAY_TOKEN"

[projects.main]
backend = "http"
url = "http://10.10.10.21:8787"
token_env = "SHUTTLE_MAIN_BACKEND_TOKEN"

[projects.local-test]
backend = "local"
repo = "/path/to/local-test"
db = "/path/to/local-test/.shuttle/shuttle.db"
```

Start the gateway with the configuration file.

```bash
SHUTTLE_GATEWAY_TOKEN=<gateway-token> \
SHUTTLE_MAIN_BACKEND_TOKEN=<backend-token> \
shuttle-gateway serve --config projects.toml
```

Write-oriented MCP tools require a `project` argument.
Read-oriented tools can use the project under `[defaults]`.

Add a project to a running gateway with the HTTP API or the `shuttle_project_add` MCP tool.

```bash
curl -X POST http://127.0.0.1:8788/api/projects \
  -H 'content-type: application/json' \
  --data '{
    "name": "extra",
    "backend": "http",
    "url": "http://10.10.10.22:8787",
    "token_env": "SHUTTLE_EXTRA_BACKEND_TOKEN"
  }'
```

Gateway OCI images and LXC archives are published through GitHub Releases.
Pull the OCI image from GHCR.

```bash
docker pull ghcr.io/f4ah6o/shuttle-gateway:<version>
```

## Event synchronization

### Cloudflare gateway

See [`docs/deploy-cloudflare.md`](./docs/deploy-cloudflare.md) to deploy the Cloudflare Worker and D1 gateway.

Persist the gateway URL and project in the repository.
Shuttle stores the environment variable name for the token, not the token value.

```bash
export SHUTTLE_GATEWAY_TOKEN=stl_...
stl sync init --url https://<gateway-host> --project my-project
```

Send and receive events.

```bash
stl sync push
stl sync pull
stl sync
```

`stl sync` pushes and then pulls.
Stable event IDs make repeated transfers idempotent.

### Files and SQLite

Move events through a JSON archive.

```bash
stl mesh export shuttle-events.json
stl mesh import shuttle-events.json
```

Synchronize directly with another Shuttle database.

```bash
stl mesh sync /path/to/peer/.shuttle/shuttle.db
```

Imported events appear in memory, message, task, and handoff projections at the destination.

## Adapter routing

Shuttle builds a project embedding from repository structure, Git metadata, and the event log.
It compares registered adapters by cosine similarity and outputs ranked selections, a merge plan, or a manifest for an external inference engine.
Shuttle does not run model inference.

```bash
stl adapter register \
  --name rust-cli \
  --base-model Qwen/Qwen2.5-Coder-7B-Instruct \
  --path /path/to/adapters/rust-cli \
  --tag rust \
  --tag cli

stl adapter list
stl adapter index
stl --json adapter select
stl --json adapter merge --top-k 3 --min-score 0.0
stl --json adapter export --top-k 3 --min-score 0.0
```

`doc2lora` writes `context.md` from repository metadata and the event log, then asks an external runner to generate an adapter.
Shuttle registers the manifest returned by the runner.

```bash
stl adapter doc2lora \
  --name project-lora \
  --base-model Qwen/Qwen2.5-Coder-7B-Instruct \
  --out-dir ./adapters/project-lora \
  --tag generated \
  --focus "adapter routing"
```

The runner is resolved from `--runner`, `SHUTTLE_DOC2LORA_RUNNER`, or `doc2lora` on `PATH`, in that order.

## Coding-agent integration

Print or install a skill for Codex or Claude Code.

```bash
stl skill print codex
stl skill install codex
stl skill print claude
stl skill install claude
```

Repository-wide agent instructions are in [`AGENTS.md`](./AGENTS.md).

Tool-specific setup is documented here:

- [Codex](./docs/codex.md)
- [Claude Code](./docs/claude-code.md)
- [OpenCode](./docs/opencode.md)
- [Codex Desktop and Claude Desktop collaboration](./docs/desktop-collaboration.md)

## Telemetry

Diagnostics go to stderr so normal and JSON output remain clean.
Set log verbosity with `RUST_LOG`.

```bash
RUST_LOG=info,shuttle_rs=debug stl context
```

Enable OpenTelemetry OTLP export with environment variables.

```bash
SHUTTLE_OTEL=1 \
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317 \
OTEL_SERVICE_NAME=stl \
RUST_LOG=info,shuttle_rs=debug \
stl app serve --addr 127.0.0.1:8787
```

Trace attributes include command and request metadata.
They exclude memory contents, message bodies, OAuth tokens, Bearer tokens, and request bodies.

## Development

Run the same checks as CI.

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
```

Use `just release-check` for the release validation sequence.

## Acknowledgements

Shuttle is inspired by [kioku-mesh](https://github.com/h-wata/kioku-mesh), which shares memory between coding agents.

Its task-coordination design also draws from [rally-rs](https://github.com/f4ah6o/rally-rs) and [agmsg](https://github.com/fujibee/agmsg).

## License

Shuttle is available under the MIT License or the Apache License 2.0.
