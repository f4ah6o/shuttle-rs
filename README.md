# shuttle-rs
<!-- bdg:begin -->
[![crates.io](https://img.shields.io/crates/v/shuttle-rs.svg)](https://crates.io/crates/shuttle-rs)
[![crates.io downloads](https://img.shields.io/crates/d/shuttle-rs.svg)](https://crates.io/crates/shuttle-rs)
[![docs.rs](https://docs.rs/shuttle-rs/badge.svg)](https://docs.rs/shuttle-rs)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](https://github.com/shuttle-rs/shuttle-rs)
[![release](https://img.shields.io/github/v/release/shuttle-rs/shuttle-rs.svg)](https://github.com/shuttle-rs/shuttle-rs/releases)
[![codecov](https://img.shields.io/codecov/c/github/shuttle-rs/shuttle-rs.svg)](https://codecov.io/gh/shuttle-rs/shuttle-rs)
[![CI](https://github.com/shuttle-rs/shuttle-rs/actions/workflows/publish.yaml/badge.svg)](https://github.com/shuttle-rs/shuttle-rs/actions/workflows/publish.yaml)
<!-- bdg:end -->

[日本語版](./README.ja.md)

`shuttle-rs` is a local-first event log for agent memory, messaging, repository
context, and coordination. The `stl` CLI stores data in `.shuttle/shuttle.db`
under the current Git repository.

## Agent Onboarding

Use [AGENTS.md](./AGENTS.md) as the canonical workflow guide for coding agents.
Tool-specific setup paths are available for [opencode](./docs/opencode.md),
[Claude Code](./docs/claude-code.md), and [Codex](./docs/codex.md). Claude Code
can also use [CLAUDE.md](./CLAUDE.md) as its conventional entrypoint.

Install the bundled Codex skill for Shuttle:

```bash
stl skill install codex
```

Preview the generated skill without writing it:

```bash
stl skill print codex
```

## Phase 1 Commands

Initialize local storage:

```bash
stl init
```

Set or inspect the local agent identity:

```bash
stl identity current
stl identity set codex
```

`SHUTTLE_AGENT` overrides the repo-local identity in `.shuttle/agent`. When
neither is set, Shuttle uses `unknown`.

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

Send and receive agent messages:

```bash
stl send claude "Please review the latest diff"
stl send --from claude codex "LGTM after the last fix"
stl inbox
stl inbox --agent claude
stl inbox --watch
stl history
```

Messages are stored in the same append-only event log as memories, tasks,
handoffs, and repository context. Use messages for transient agent-to-agent
communication, tasks for trackable work, handoffs for ownership transfer, and
typed memories for durable outcomes.

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

Promote an important message into durable project state:

```bash
stl decide --from-message <message-id>
stl task create --from-message <message-id>
stl handoff request claude --from-message <message-id>
```

Expose Shuttle over HTTP MCP:

```bash
stl app serve --addr 127.0.0.1:8787
```

The app serves a small JSON dashboard at `/` and API endpoints for dashboard
state, inbox, tasks, memories, and repository context. It also exposes the MCP
endpoint at `/mcp`.

Configure MCP clients to use the `/mcp` endpoint:

```json
{
  "mcpServers": {
    "shuttle": {
      "url": "http://127.0.0.1:8787/mcp"
    }
  }
}
```

Set `SHUTTLE_MCP_BEARER_TOKEN` before starting `stl app serve` to require
`Authorization: Bearer <token>` on MCP requests. When the variable is unset,
local MCP remains unauthenticated.

Pass `--public-url` to `stl app serve` when the app is already behind a public
HTTPS endpoint and should publish OAuth metadata:

```bash
SHUTTLE_OAUTH_ADMIN_TOKEN=<admin-token> \
stl app serve --addr 127.0.0.1:8787 --public-url https://shuttle.example.com
```

Expose Shuttle as a remote MCP server for web chat clients with a Cloudflare
Named Tunnel:

```bash
SHUTTLE_OAUTH_ADMIN_TOKEN=<admin-token> \
CLOUDFLARE_TUNNEL_TOKEN=<cloudflare-tunnel-token> \
stl app tunnel --public-url https://shuttle.example.com
```

Configure ChatGPT or Claude with `https://shuttle.example.com/mcp`. The tunnel
token is read only from the environment, so use a secret manager or runtime
injection instead of putting it in shell history. The public URL must match the
Cloudflare Tunnel hostname that forwards to `http://127.0.0.1:8787`.

The MCP server provides memory, message, task, handoff, repository context,
repository status, changed-file, and diff tools. Tool aliases such as `remember`
and namespaced tools such as `shuttle_memory_store` call the same local event
log.

## Telemetry

Shuttle emits `tracing` diagnostics to stderr, preserving stdout for normal CLI
and `--json` output. Set `RUST_LOG` to control local log verbosity:

```bash
RUST_LOG=info,shuttle_rs=debug stl context
```

OpenTelemetry trace export is disabled by default. Enable OTLP export by setting
`SHUTTLE_OTEL=1` or an OTLP endpoint before running `stl`, `stl app serve`, or
`shuttle-gateway`:

```bash
SHUTTLE_OTEL=1 \
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317 \
OTEL_SERVICE_NAME=stl \
RUST_LOG=info,shuttle_rs=debug \
stl app serve --addr 127.0.0.1:8787
```

HTTP app and gateway requests are traced automatically. Shuttle records command
and request metadata, but avoids recording memory contents, message bodies,
OAuth tokens, bearer tokens, or request bodies as span attributes.

## Multi-project Gateway

For web chat clients that should use one MCP server across several local
repositories, run `shuttle-gateway`. The gateway is the MCP, auth, and project
routing boundary. Each local project runs in-process against its
`.shuttle/shuttle.db` — the gateway is standalone and needs no separate `stl`
binary — or a project can point at a repo-local `stl app serve` process over
HTTP.

The remote deployment model is:

```text
gateway host / LXC
└─ shuttle-gateway

project environment
└─ stl app serve
   └─ repo + .shuttle/shuttle.db
```

Create a project config from `examples/projects.example.toml`. Use
`backend = "http"` with `url` for remote project environments, or
`backend = "local"` with an absolute `repo` path for compatibility mode. If
`backend` is omitted and `repo` is present, the project is treated as local.

Run the gateway with the config:

```bash
shuttle-gateway serve --config projects.toml
```

Check installed binary versions:

```bash
stl version
shuttle-gateway version
stl --json version
```

Local backends run in-process by default, so no external `stl` binary is
required. Pass `--stl /path/to/stl` to opt into executing local projects through
that external binary instead, and `--timeout <seconds>` to set local subprocess
and HTTP backend timeouts. `--addr` can override a single-listener config, but a
config with multiple `[[listeners]]` owns its listener addresses.

Listeners separate ingress/auth policy from project backend type. A public Web
Chat listener should use OAuth, while private LAN/Tailscale/local listeners can
use bearer auth or loopback-only unauthenticated access:

```toml
[[listeners]]
name = "public"
addr = "127.0.0.1:8787"
auth = "oauth"
public_url = "https://shuttle.example.com"
oauth_admin_token_env = "SHUTTLE_OAUTH_ADMIN_TOKEN"

[[listeners]]
name = "private"
addr = "127.0.0.1:8788"
auth = "bearer"
bearer_token_env = "SHUTTLE_GATEWAY_TOKEN"
```

The gateway requires an explicit `project` argument for writes such as
`shuttle_remember` and `shuttle_task_create`. Reads may use the configured
default project.

Add projects to a running gateway with `POST /api/projects` or the MCP tool
`shuttle_project_add`. Additions are written to `projects.toml` and are
available immediately to all listeners. If the requested name is already in use,
the gateway chooses the next available suffix such as `extra-2`.

```bash
curl -X POST http://127.0.0.1:8788/api/projects \
  -H 'content-type: application/json' \
  --data '{
    "name": "extra",
    "backend": "http",
    "url": "http://10.10.10.22:8787",
    "token_env": "SHUTTLE_EXTRA_BACKEND_TOKEN",
    "make_current": true
  }'
```

For HTTP backends, start a repo-local app server in the project environment:

```bash
SHUTTLE_MCP_BEARER_TOKEN=<backend-token> \
stl app serve --addr 127.0.0.1:8787
```

Then configure the gateway project with the URL and backend token environment
variable:

```toml
[projects.main]
backend = "http"
url = "http://10.10.10.21:8787"
token_env = "SHUTTLE_MAIN_BACKEND_TOKEN"
```

Register the public listener URL with remote MCP clients such as ChatGPT or
Claude web connectors:

```json
{
  "mcpServers": {
    "shuttle-gateway": {
      "url": "https://shuttle.example.com/mcp"
    }
  }
}
```

Run OAuth listeners with the owner-approval token injected at runtime:

```bash
SHUTTLE_OAUTH_ADMIN_TOKEN=<admin-token> \
shuttle-gateway serve --config projects.toml
```

OAuth client registrations, authorization codes, and access tokens are stored in
gateway-local SQLite databases. Backend tokens and OAuth admin tokens should be
provided by a secret manager or runtime-injected environment variables.

The standard `v*` release workflow publishes the Rust crate and gateway
artifacts from the same tag; a separate gateway-specific release tag is not
required.

For LXC-oriented gateway hosts, `v*` release tags publish archives named
`shuttle-gateway-lxc-<target>.tar.gz`. Each archive contains
`bin/shuttle-gateway`, `bin/stl`, LXC config examples, a systemd unit, and
`install.sh`. The installer uses `/usr/local/bin`, `/etc/shuttle-gateway`, and
`/var/lib/shuttle-gateway` by default. Edit `projects.toml` and
`shuttle-gateway.env` after installation; do not store real token values in the
example files.

The same `v*` releases also publish OCI images and per-architecture OCI layout
archives. Pull the registry image from GHCR:

```bash
docker pull ghcr.io/f4ah6o/shuttle-gateway:<version>
```

Run it with mounted config and state directories:

```bash
docker run --rm \
  -p 8787:8787 \
  -v /path/to/shuttle-gateway:/etc/shuttle-gateway \
  -v /path/to/shuttle-gateway-state:/var/lib/shuttle-gateway \
  -e SHUTTLE_OAUTH_ADMIN_TOKEN=<admin-token> \
  -e SHUTTLE_MAIN_BACKEND_TOKEN=<backend-token> \
  ghcr.io/f4ah6o/shuttle-gateway:<version>
```

Apple's `container` tool can pull the same OCI image or load a release archive:

```bash
container image pull ghcr.io/f4ah6o/shuttle-gateway:<version>
container image load --input shuttle-gateway-oci-linux-arm64.tar
container run \
  -p 8787:8787 \
  -v /path/to/shuttle-gateway:/etc/shuttle-gateway \
  -v /path/to/shuttle-gateway-state:/var/lib/shuttle-gateway \
  ghcr.io/f4ah6o/shuttle-gateway:<version>
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

## Adapter router

Shuttle can act as a lightweight, local-first project intelligence layer that
selects coding adapters (such as LoRA/PEFT adapters) for the current repository.
It builds a deterministic project embedding from repository structure, git
metadata, and the local event log, scores adapters from a local registry by
cosine similarity, and exports a routing manifest for external inference engines.
Routing itself never runs a model — Shuttle only scores and exports adapters that
already exist.

```bash
# Register adapters in the local registry (embeddings are computed from the
# adapter description, or supplied with --embedding '<json array>').
stl adapter register --name rust-cli \
  --base-model Qwen/Qwen2.5-Coder-7B-Instruct \
  --path /path/to/adapters/rust-cli --tag rust --tag cli
stl adapter list

# Build the project embedding, inspect the selection, and produce a plan.
stl adapter index            # build and cache the project embedding
stl adapter select --json    # ranked adapters + detected project type
stl adapter merge --json     # deterministic weights that sum to 1.0
stl adapter export --json    # runtime manifest (base model + adapter paths)
```

`merge` and `export` accept `--top-k` and `--min-score` to control how many
adapters are retained and the minimum similarity required. `export --format`
currently supports `json`; runtime-specific presets (PEFT, vLLM, MLX) are planned.

### doc-to-lora generation

Beyond routing to existing adapters, Shuttle can generate one from the current
project's accumulated knowledge by driving an external
[doc-to-lora](https://github.com/SakanaAI/doc-to-lora) runner. Shuttle assembles a
context document from repository metadata and the local event log (decisions,
facts, patterns, observations, bugs, tasks, handoffs, and memories), hands it to
the runner, and registers the produced adapter so it is immediately routable.
Shuttle still never runs model inference itself — the external runner performs the
generation.

```bash
stl adapter doc2lora \
  --name project-lora \
  --base-model Qwen/Qwen2.5-Coder-7B-Instruct \
  --out-dir ./adapters/project-lora \
  --tag generated \
  --focus "adapter routing"   # optional: bias and annotate the context document
```

Shuttle writes the context document to `<out-dir>/context.md` and invokes the
runner as:

```text
<runner> generate --base-model <model> --document <out-dir>/context.md \
  --output <out-dir> --name <name>
```

The runner must produce the adapter under `--output` and print a JSON manifest to
stdout. `path` is required; `base_model` and `name` are optional and fall back to
the requested values:

```json
{ "path": "./adapters/project-lora", "base_model": "Qwen/Qwen2.5-Coder-7B-Instruct", "name": "project-lora" }
```

The runner program is resolved from `--runner`, then the
`SHUTTLE_DOC2LORA_RUNNER` environment variable, then `doc2lora` on `PATH`. The
registered adapter is embedded from its source document, so it sits in the same
space as the project embedding and is selected by `stl adapter select` right away.

## Acknowledgements

Shuttle is inspired by [kioku-mesh](https://github.com/h-wata/kioku-mesh), a
shared memory system for AI coding agents across tools and machines.

Shuttle also builds on ideas from [rally-rs](https://github.com/f4ah6o/rally-rs),
which in turn builds on [agmsg](https://github.com/fujibee/agmsg).
