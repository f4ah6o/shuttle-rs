# Deploying the shuttle-gateway Worker to Cloudflare

[日本語版](./deploy-cloudflare.ja.md)

This guide walks through deploying the cloud shuttle-gateway — the Cloudflare
Worker in [`workers/shuttle-gateway/`](../workers/shuttle-gateway/) — from a
fresh Cloudflare account to a running gateway with projects, tokens, and local
repositories syncing against it.

The Worker is a stateless MCP endpoint plus a resource-oriented HTTP API backed
by Cloudflare D1, which owns the durable shared event store. It is independent
of the Rust `shuttle-gateway` binary; for the self-hosted Rust gateway, see the
"Multi-project Gateway" section in the repository [README](../README.md) and
the `packaging/` directory.

## Prerequisites

- A Cloudflare account with Workers and D1 enabled (the free tier is enough to
  start).
- Node.js 20 or newer and npm.
- The repository checked out locally.

All commands below run from the Worker directory:

```bash
cd workers/shuttle-gateway
npm install
```

Authenticate wrangler with your Cloudflare account (opens a browser):

```bash
npx wrangler login
```

For non-interactive environments (CI), set `CLOUDFLARE_API_TOKEN` (and
`CLOUDFLARE_ACCOUNT_ID` if the token spans multiple accounts) instead of
logging in. Keep the token in a secret manager or environment injection —
never commit it.

## 1. Create the D1 database

```bash
npx wrangler d1 create shuttle
```

The command prints a `database_id`. Copy it into
[`wrangler.toml`](../workers/shuttle-gateway/wrangler.toml), replacing the
placeholder:

```toml
[[d1_databases]]
binding = "DB"
database_name = "shuttle"
database_id = "<the id printed by wrangler d1 create>"
migrations_dir = "migrations"
```

## 2. Configure vars

Still in `wrangler.toml`:

- `PUBLIC_URL` — the public base URL of the deployed Worker (for example
  `https://shuttle-gateway.<your-subdomain>.workers.dev`, or your custom
  domain). It is surfaced in MCP/OAuth metadata.
- `ADMIN_OWNER_ID` — the owner id associated with the bootstrap admin token.
  The default `owner-local` is fine; it is only used to mint the first scoped
  tokens and create the first projects.

If you don't know the `workers.dev` URL yet, you can deploy once (step 5),
note the URL wrangler prints, set `PUBLIC_URL`, and deploy again.

## 3. Apply migrations

```bash
npm run migrate:remote
```

This applies `migrations/0001_init.sql` (and any later migrations) to the
remote D1 database. Re-run it after pulling changes that add migrations —
applying is idempotent per migration.

## 4. Set the bootstrap admin secret

Generate a strong one-time token and store it as a Worker secret:

```bash
openssl rand -hex 32   # generate a value; keep it somewhere safe temporarily
npx wrangler secret put ADMIN_BOOTSTRAP_TOKEN
```

`wrangler secret put` prompts for the value on stdin, so the token never
appears in shell history or in the repository.

The bootstrap token is genuinely one-time: it works only until the first
`admin`-scoped token is minted, then it is rejected forever.

## 5. Deploy

```bash
npm run deploy
```

Wrangler prints the deployed URL. Verify the Worker is healthy:

```bash
curl -s https://<gateway-host>/api/health
curl -s https://<gateway-host>/mcp        # MCP health
```

## 6. Bootstrap tokens and projects

Use the bootstrap token exactly once to mint a persistent admin token. Minting
the admin token consumes the bootstrap token, so save the response.

```bash
URL=https://<gateway-host>

# 1. Mint a persistent admin token with the bootstrap token (one-time).
curl -sX POST "$URL/api/tokens" -H "authorization: Bearer $BOOTSTRAP" \
  -H 'content-type: application/json' -d '{"scopes":["admin"]}'
# -> { "token": "stl_...", "scopes": ["admin"], ... }   save this

# 2. Create a project with the admin token.
curl -sX POST "$URL/api/projects" -H "authorization: Bearer $ADMIN" \
  -H 'content-type: application/json' -d '{"slug":"my-project"}'

# 3. Mint project-scoped tokens for local agents.
curl -sX POST "$URL/api/tokens" -H "authorization: Bearer $ADMIN" \
  -H 'content-type: application/json' \
  -d '{"project":"my-project","scopes":["read","write"]}'
```

Tokens are personal access tokens scoped `read`/`write`/`admin` and are stored
server-side only as SHA-256 hashes — a lost token cannot be recovered, only
re-minted.

## 7. Connect local repositories

In each repository that should sync against the gateway, configure `stl sync`
with a project-scoped token from step 6:

```bash
export SHUTTLE_GATEWAY_TOKEN=stl_...   # scoped PAT minted by the gateway
stl sync init --url https://<gateway-host> --project my-project
stl sync push   # upload local events (idempotent by event id)
stl sync pull   # download gateway events into this workspace
stl sync        # both: push, then pull
```

`stl sync init` writes `.shuttle/remote.json` (URL, project, and optionally
the token env var name via `--token-env`); the token itself is never stored.
See "Cloud Sync (Cloudflare gateway)" in [AGENTS.md](../AGENTS.md) for the
full sync semantics.

MCP clients that speak Streamable HTTP can point at
`https://<gateway-host>/mcp` with an `Authorization: Bearer stl_...` header.
OAuth 2.1 for ChatGPT/Claude.ai web clients is not yet implemented (tracked in
issue #46), so PAT-based clients only for now.

## Local development

Run the Worker locally against a local D1 emulation:

```bash
npm run migrate:local
npm run dev
```

`npm test` runs the test suite against Node's built-in SQLite through the same
storage port the D1 adapter implements, and `npm run typecheck` type-checks the
Worker.

## Updating a deployment

```bash
git pull
npm install
npm run migrate:remote   # applies only new migrations
npm run deploy
```

Deploys are atomic on Cloudflare's side; D1 data, secrets, and minted tokens
survive redeploys.

## Troubleshooting

- **`database_id` errors on deploy or migrate** — `wrangler.toml` still has the
  all-zero placeholder id. Paste the id from `wrangler d1 create shuttle`
  (or `wrangler d1 list`).
- **Bootstrap token rejected (401)** — an admin token has already been minted,
  which permanently consumes the bootstrap token. Use the persistent admin
  token; if it is lost, set a new `ADMIN_BOOTSTRAP_TOKEN` secret only after
  reviewing the grants in D1.
- **`node:` module resolution errors at runtime** — make sure
  `compatibility_flags = ["nodejs_compat"]` is present in `wrangler.toml`.
- **`stl sync` authentication failures** — verify `SHUTTLE_GATEWAY_TOKEN` is
  exported in the current shell and that the token's project matches the
  `--project` slug used at `stl sync init`.
