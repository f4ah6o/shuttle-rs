# Agent Guide

## README Rules

`README.ja.md` and `README.md` describe behavior that exists in the checked-in code and information that users need to operate it.

Do not use either README for excuses, missing-function explanations, release-stage labels, roadmaps, or desired future behavior.
When a required capability is absent, open a GitHub issue with the expected behavior and acceptance criteria instead of placing it in a README.

Treat these words in README prose as warning signs:

- `current`
- `MVP`
- `PoC`
- `Phase`

Replace release-stage language with concrete commands, inputs, outputs, storage behavior, authentication requirements, and supported deployment paths.

Write `README.ja.md` before editing `README.md`.
Apply the [`japanese-tech-writing`](https://github.com/f4ah6o/tech-write-ja/tree/main/skills/japanese-tech-writing) rules to the Japanese text, including one sentence per line, one topic per paragraph, specific headings, restrained emphasis, and removal of empty summary language.
Keep the Japanese and English README structures aligned, and place a mutual language link near the top of both files.

## Repository Purpose

This repository provides `stl`, a local-first event log CLI for agent memory, repository context, task coordination, handoffs, messaging, workflow execution, mesh and cloud synchronization, adapter routing, and MCP access.

It also provides `shuttle-gateway`, which exposes several Shuttle projects through shared MCP listeners with per-listener authentication.

## Setup

Build the workspace.

```bash
cargo build
```

Local mode stores data in `.shuttle/shuttle.db` at the Git repository root.
Initialize the local database with:

```bash
cargo run --bin stl -- init
```

After installing `stl`, the same initialization is:

```bash
stl init
```

Cloud-first mode is selected when `.shuttle/shuttle.db` is absent and `SHUTTLE_GATEWAY_URL` is set.
It uses the Cloudflare Worker and D1 project selected by `SHUTTLE_GATEWAY_PROJECT`.
Set the gateway URL, project, and project-scoped token before running ordinary commands:

```bash
export SHUTTLE_GATEWAY_URL=https://<gateway-host>
export SHUTTLE_GATEWAY_PROJECT=my-project
export SHUTTLE_GATEWAY_TOKEN=stl_...
stl context
```

`stl init` refuses to create `.shuttle/shuttle.db` in cloud-first mode.

## Before Starting Work

Read the repository state and coordination queue before making changes.

```bash
stl context
stl inbox
stl recall "task"
stl task list
```

Use JSON output when another tool needs structured data.

```bash
stl --json context
stl --json inbox
stl --json recall "task"
stl --json task list
```

Set a repository-local agent identity when the runtime does not provide `SHUTTLE_AGENT`.

```bash
stl identity set codex
```

## During Work

Record durable information with the event type that matches its role.

```bash
stl remember "important project note"
stl observe "what changed"
stl decide "important implementation decision"
stl pattern "repeatable workflow or design pattern"
stl fact "stable project fact"
stl bug "known issue or failing behavior"
```

Recall by query and narrow by type when useful.

```bash
stl recall "SQLite decision"
stl recall "SQLite decision" --type decision
```

Shuttle attaches repository metadata to events, including repository path, remote, branch, commit, dirty state, and dirty file names.

## Task Coordination

Tasks are projected from append-only events.

```bash
stl task create "Implement feature"
stl task list
stl task claim <task-id>
stl task update <task-id> "Progress update"
stl task done <task-id>
```

Use `stl context` to inspect open tasks, claimed tasks, pending handoffs, recent decisions, related memories, messages, and inbox entries together.

## Handoffs and Messages

Transfer work between agents with handoffs.

```bash
stl handoff request claude "Please continue this branch"
stl handoff list
stl handoff accept <handoff-id>
stl handoff done <handoff-id>
```

Use messages for transient communication.

```bash
stl send codex "Please review the latest diff"
stl inbox
stl inbox --watch
stl history
```

Promote a message when its content belongs in durable project state.

```bash
stl decide --from-message <message-id>
stl task create --from-message <message-id>
stl handoff request claude --from-message <message-id>
```

## Repository Workflows

Repository-owned workflows live in `shuttle.workflows.toml`.

```bash
stl workflow list
stl workflow show <workflow-id>
stl workflow start <workflow-id>
stl workflow status <run-id>
```

Claim and complete steps explicitly.

```bash
stl workflow step claim <run-id> <step-id>
stl workflow step complete <run-id> <step-id> --output '{}'
```

An interrupted step remains claimed.
After checking the previous claim, another agent takes it over with a reason:

```bash
stl workflow step claim <run-id> <step-id> \
  --takeover \
  --reason "Previous agent was interrupted"
```

A taken-over `non_idempotent` step enters `needs_reconcile`.
Inspect the external system instead of repeating the operation, then record the observed result:

```bash
stl workflow reconcile <run-id> <step-id> --output '{}'
```

## MCP

Start the repository-local HTTP MCP server.

```bash
stl app serve --addr 127.0.0.1:8787
```

Configure MCP clients with `http://127.0.0.1:8787/mcp`.

Set `SHUTTLE_MCP_BEARER_TOKEN` before startup to require Bearer authentication.
Use `SHUTTLE_OAUTH_ADMIN_TOKEN` with `--public-url` for OAuth-enabled public endpoints.

## Mesh and Cloud Synchronization

Move events through archives or another SQLite database.

```bash
stl mesh export shuttle-events.json
stl mesh import shuttle-events.json
stl mesh sync /path/to/peer/.shuttle/shuttle.db
```

Configure a Cloudflare gateway and synchronize the local event log.

```bash
export SHUTTLE_GATEWAY_TOKEN=stl_...
stl sync init --url https://<gateway-host> --project my-project
stl sync push
stl sync pull
stl sync
```

`stl sync init` writes `.shuttle/remote.json`.
The file stores the gateway URL, project, and optional token environment-variable name, but not the token value.

## Adapter Routing

Register adapters, index the repository, and export routing results.

```bash
stl adapter register \
  --name rust-cli \
  --base-model Qwen/Qwen2.5-Coder-7B-Instruct \
  --path /path/to/adapters/rust-cli \
  --tag rust \
  --tag cli

stl adapter index
stl --json adapter select
stl --json adapter merge
stl --json adapter export
```

`stl adapter doc2lora` creates a context document and invokes an external runner.
Shuttle selects and registers adapters but does not run model inference.

## Verification

Run the checks used by CI.

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
```

Documentation-only changes still require checking links, command spelling, language parity, and the README rules above.
