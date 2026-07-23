# shuttle-gateway (Cloudflare Worker)

A cloud-hostable Shuttle gateway that exposes shared agent memory over a
stateless MCP endpoint and a resource-oriented HTTP API, backed by Cloudflare
D1. It implements the first delivery slices of
[issue #46](https://github.com/f4ah6o/shuttle-rs/issues/46): the gateway owns
the durable shared event store and treats projects as logical identities rather
than filesystem locations.

This Worker is independent of the Rust `stl`/`shuttle-gateway` binaries. The
local Rust CLI remains responsible for inspecting repositories and capturing
local context; the cloud Worker never reads a server-side filesystem or calls
Git.

## What it does

- **Cloud-owned shared memory.** D1 is the authoritative append-only event log
  (`events` + `event_tags`). A project is usable with no repository path, local
  agent, or backend URL.
- **Logical projects.** `projects` and `workspaces` carry stable UUIDs.
  Repository metadata is stored verbatim from a client context envelope.
- **Per-request project selection.** Every MCP tool and API endpoint resolves an
  explicit project and authorizes it per request. There is no process-global
  "current project", so one client's selection cannot affect another's.
- **Authorization is a type-level boundary.** Project-scoped services accept an
  `AuthorizedProject` (and writes an `AuthorizedProject<"write">`), never a raw
  `project_id`. The only way to obtain one is `authorize(db, principal, selector,
  scope)`, which resolves and authorizes in one step. A new endpoint therefore
  cannot reach the repository without passing authorization, and a read-only
  capability won't type-check against a write service. Repository methods stay
  free of authorization logic.
- **Canonical identity `(project_id, event_id)`.** A client-supplied event id is
  project-scoped, not globally unique. Replaying an id within a project is a
  no-op that reports `deduplicated: true`, while the same id in another project
  is a distinct event — cross-project collisions are impossible. This keeps
  retries/imports idempotent and future export/import/sharding simple.
- **Scoped credentials.** Local agents authenticate with Cloudflare Access
  Service Auth plus scoped personal access tokens (`read`/`write`/`admin`),
  stored as SHA-256 hashes in `project_grants`. PAT grants carry the
  server-side agent and client-instance identity used by claims.
  The bootstrap admin token is genuinely one-time: it works only until the first
  admin token is minted, then it is rejected.

## Endpoints

```
GET    /api/health                                    authenticated
POST   /mcp                                           Streamable HTTP MCP
GET    /mcp                                            MCP health
POST   /api/tokens                                    mint scoped PAT (admin)
POST   /api/projects                                  create project (admin)
GET    /api/projects                                  list projects
POST   /api/projects/{project}/workspaces
GET    /api/projects/{project}/tasks
POST   /api/projects/{project}/tasks
POST   /api/projects/{project}/tasks/{id}/claim       atomic claim
POST   /api/projects/{project}/tasks/{id}/update
POST   /api/projects/{project}/tasks/{id}/done
POST   /api/projects/{project}/workflows/{run}/steps/{step}/claim
POST   /api/projects/{project}/events                 append (idempotent; optional created_at)
GET    /api/projects/{project}/events                 ?limit=&event_type=&before=<created_at>|<id>
POST   /api/projects/{project}/recall
POST   /api/projects/{project}/context-snapshots
GET    /api/projects/{project}/context-snapshots/latest
```

MCP tools (`tools/list`, `tools/call`) call the same application services as the
API: `shuttle_projects`, `shuttle_project_create`, `shuttle_remember`,
`shuttle_recall`, `shuttle_context`, `shuttle_context_publish`,
`shuttle_task_create`, `shuttle_task_list`, `shuttle_task_update`,
`shuttle_task_claim`, `shuttle_task_done`.

## Authentication

The production deployment uses Cloudflare Access Managed OAuth for interactive
MCP clients. Access restricts the configured gateway hostname to a dedicated
Access group, and the Worker verifies the signed `Cf-Access-Jwt-Assertion`
before mapping the user to the configured shared tenant with `read`/`write`
access. The stable Access JWT `sub` becomes the event `agent_id`.

Headless agents and local `stl` commands continue to use Cloudflare Access
Service Auth plus a project-scoped Shuttle PAT. Admin operations remain PAT
only; OAuth users cannot create projects or mint tokens.

For ChatGPT web MCP clients, allow the callback-ID pattern
`https://chatgpt.com/connector/oauth/*` in Cloudflare Access Managed OAuth.
Retain the older exact callback URI
`https://chatgpt.com/connector_platform_oauth_redirect` for compatibility.

## Develop

```bash
npm install
npm run typecheck
npm test          # runs against Node's built-in SQLite via the storage port
```

Tests exercise the same services and HTTP/MCP handlers that run on the Worker,
using an in-memory `node:sqlite` implementation of the `Database` storage port
(`src/database.ts`). The D1 adapter (`D1Database_`) implements the same port in
production.

## Deploy

For a full step-by-step guide (wrangler auth, the local `wrangler.toml`
configuration,
verification, and connecting local repositories), see
[docs/deploy-cloudflare.md](../../docs/deploy-cloudflare.md). Short version:

```bash
# copy the sample and set the deployment-specific values
cp wrangler.toml.sample wrangler.toml

# create the D1 database and copy its id into wrangler.toml
wrangler d1 create shuttle

# apply migrations
npm run migrate:remote

# configure a Cloudflare Access self-hosted app for the chosen hostname,
# enable Managed OAuth, and set ACCESS_TEAM_DOMAIN/AUD in wrangler.toml

# set the one-time bootstrap admin token, then deploy
wrangler secret put ADMIN_BOOTSTRAP_TOKEN
npm run deploy
```

The sample configuration publishes only the custom domain
(`workers_dev = false`). Protect the whole hostname in Cloudflare Access,
including `/api/health`; the Rust client sends the Access
service-auth headers from `CLOUDFLARE_ACCESS_CLIENT_ID` and
`CLOUDFLARE_ACCESS_CLIENT_SECRET` in addition to the PAT.

After deploy, use the bootstrap token once to mint a persistent admin token.
Minting an admin token consumes the bootstrap token: it is rejected afterwards,
so use the new admin token for everything else. Delete the bootstrap secret after
the admin token has been stored safely.

```bash
ACCESS_HEADERS=(
  -H "CF-Access-Client-Id: $CLOUDFLARE_ACCESS_CLIENT_ID"
  -H "CF-Access-Client-Secret: $CLOUDFLARE_ACCESS_CLIENT_SECRET"
)

# 1. Mint a persistent admin token with the bootstrap token (one-time).
curl -sX POST "$URL/api/tokens" "${ACCESS_HEADERS[@]}" -H "authorization: Bearer $BOOTSTRAP" \
  -H 'content-type: application/json' -d '{"scopes":["admin"]}'
# -> { "token": "stl_...", "scopes": ["admin"], ... }   save this

# 2. Create a project with the admin token.
curl -sX POST "$URL/api/projects" "${ACCESS_HEADERS[@]}" -H "authorization: Bearer $ADMIN" \
  -H 'content-type: application/json' -d '{"slug":"my-project"}'

# 3. Mint project-scoped tokens for local agents.
curl -sX POST "$URL/api/tokens" "${ACCESS_HEADERS[@]}" -H "authorization: Bearer $ADMIN" \
  -H 'content-type: application/json' \
  -d '{"project":"my-project","scopes":["read","write"],"agent_id":"linux-codex","client_instance_id":"linux-codex"}'
```

## Syncing repo-local event logs (migration / local-mode compatibility)

The `stl sync` command imports/exports repo-local `.shuttle/shuttle.db` event
logs against this Worker (see "Cloud Sync" in the repository `AGENTS.md`).
Two Worker features support it:

- **Client-supplied `created_at`.** The append body accepts an optional
  ISO-8601 `created_at` so imported history keeps its original ordering;
  server time is used when absent.
- **Keyset pagination.** `GET .../events` accepts
  `before=<created_at>|<id>` matching the listing order
  (`created_at DESC, id DESC`) and responds with
  `{ events, has_more, next_before }`, so pulls are not capped by the
  500-event page limit.

## Not yet implemented (tracked in #46)

- R2 offload for large snapshots/archives and Queue/Vectorize enrichment.
- Richer handoff/workflow resource endpoints beyond the canonical event API.
