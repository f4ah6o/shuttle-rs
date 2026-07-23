# Deploying the shuttle-gateway Worker to Cloudflare

[日本語版](./deploy-cloudflare.ja.md)

This guide walks through deploying the cloud shuttle-gateway — the Cloudflare
Worker in [`workers/shuttle-gateway/`](../workers/shuttle-gateway/) — in a
Cloudflare account with a custom domain, Cloudflare Access, projects, tokens,
and local repositories syncing against it.

The Worker is a stateless MCP endpoint plus a resource-oriented HTTP API backed
by Cloudflare D1, which owns the durable shared event store. It is independent
of the Rust `shuttle-gateway` binary; for the self-hosted Rust gateway, see the
"Multi-project Gateway" section in the repository [README](../README.md) and
the `packaging/` directory.

## Prerequisites

- A Cloudflare account with Workers and D1 enabled.
- A zone for the chosen gateway hostname active in that same account.
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

The command prints a `database_id`. Copy the sample to a local configuration
file, then replace its placeholders:

```bash
cp wrangler.toml.sample wrangler.toml
```

```toml
[[d1_databases]]
binding = "DB"
database_name = "shuttle"
database_id = "<the id printed by wrangler d1 create>"
migrations_dir = "migrations"
```

## 2. Configure vars

Still in the local, untracked `wrangler.toml`:

- `PUBLIC_URL` — the base URL of the deployed Worker. For this example use
  `https://shuttle.example.com`; it is surfaced in MCP/OAuth metadata.
- `ADMIN_OWNER_ID` — the stable shared tenant owner id. This example uses
  `example-owner`.
- `ACCESS_TEAM_DOMAIN` — the Cloudflare Access team issuer URL, for example
  `https://<team-name>.cloudflareaccess.com`.
- `ACCESS_APPLICATION_AUD` — the Application Audience (AUD) tag for the Access
  application protecting this hostname.

The sample configuration uses `workers_dev = false` and the custom domain
`shuttle.example.com`. Keep the domain route and the D1 account aligned; do not
enable a public `workers.dev` endpoint for a private deployment.

Create a Cloudflare Access self-hosted application for
`shuttle.example.com/*` before the first real client connection. Configure two
allow policies:

1. A dedicated Access group, for human MCP clients.
2. Service Auth for the service tokens assigned to allowed terminals and
   agents.

If the Access login page exposes **Send login code** (One-time PIN), add the
intended email address or email domain as an additional Include rule. The
`cloudflare_account_member` selector is evaluated by the Cloudflare identity
provider and does not by itself authorize an OTP login.

In the application's Advanced settings, enable Managed OAuth. Copy the Access
team domain and the application's AUD tag into `ACCESS_TEAM_DOMAIN` and
`ACCESS_APPLICATION_AUD` in the local `wrangler.toml`. The Worker verifies the signed
`Cf-Access-Jwt-Assertion` itself; do not trust the header without signature,
issuer, and audience validation. Protect `/api/health` too; health is not a
public liveness endpoint.

For the ChatGPT web MCP client, add this URI pattern to Managed OAuth's
**Allowed redirect URIs**:

```text
https://chatgpt.com/connector/oauth/*
```

ChatGPT uses a callback ID below `/connector/oauth/`. Keep the pattern scoped to
that path instead of allowing the whole `chatgpt.com` origin. For older ChatGPT
connector flows, also retain the exact URI below:

```text
https://chatgpt.com/connector_platform_oauth_redirect
```

Cloudflare rejects Dynamic Client Registration when the client's redirect URI
is not in this allowlist.

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
curl -s https://shuttle.example.com/api/health \
  -H "CF-Access-Client-Id: $CLOUDFLARE_ACCESS_CLIENT_ID" \
  -H "CF-Access-Client-Secret: $CLOUDFLARE_ACCESS_CLIENT_SECRET" \
  -H "authorization: Bearer $SHUTTLE_GATEWAY_TOKEN"
curl -s https://shuttle.example.com/mcp \
  -H "CF-Access-Client-Id: $CLOUDFLARE_ACCESS_CLIENT_ID" \
  -H "CF-Access-Client-Secret: $CLOUDFLARE_ACCESS_CLIENT_SECRET" \
  -H "authorization: Bearer $SHUTTLE_GATEWAY_TOKEN"        # MCP health
```

## 6. Bootstrap tokens and projects

Use the bootstrap token exactly once to mint a persistent admin token. Minting
the admin token consumes the bootstrap token, so save the response.

```bash
URL=https://shuttle.example.com
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
  -H 'content-type: application/json' -d '{"slug":"example-project"}'

# 3. Mint project-scoped tokens for local agents.
curl -sX POST "$URL/api/tokens" "${ACCESS_HEADERS[@]}" -H "authorization: Bearer $ADMIN" \
  -H 'content-type: application/json' \
  -d '{"project":"example-project","scopes":["read","write"],"agent_id":"example-agent","client_instance_id":"example-agent"}'
```

Tokens are personal access tokens scoped `read`/`write`/`admin` and are stored
server-side only as SHA-256 hashes — a lost token cannot be recovered, only
re-minted.

## 7. Connect local repositories (migration / local-mode compatibility)

`stl sync` remains for migrating an existing repo-local `.shuttle` database and
for local-mode compatibility. Configure it with the project-scoped token from
step 6 only during that migration:

```bash
export SHUTTLE_GATEWAY_TOKEN=stl_...   # scoped PAT minted by the gateway
stl sync init --url https://shuttle.example.com --project example-project
stl sync push   # upload local events (idempotent by event id)
stl sync pull   # download gateway events into this workspace
stl sync        # both: push, then pull
```

`stl sync init` writes `.shuttle/remote.json` (URL, project, and optionally
the token env var name via `--token-env`); the token itself is never stored.
For each project, this is a one-time migration path. After the local
`.shuttle` database is verified against D1 and removed, `stl` automatically
uses cloud-first mode from `SHUTTLE_GATEWAY_URL`,
`SHUTTLE_GATEWAY_PROJECT`, `SHUTTLE_GATEWAY_TOKEN`, and
`SHUTTLE_CLIENT_INSTANCE_ID`; it does not create an offline local fallback.
Use the project's own migration runbook for the freeze, backup, count
verification, and deletion sequence.

Headless MCP clients and local repositories use
`https://shuttle.example.com/mcp` with both the Cloudflare Access Service Auth
headers and an `Authorization: Bearer stl_...` header. Interactive MCP clients
such as ChatGPT or Claude.ai discover and use Cloudflare Access Managed OAuth;
they do not receive a Shuttle PAT. OAuth users are read/write within the shared
configured tenant, while admin operations remain PAT-only.

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

- **`database_id` errors on deploy or migrate** — the local `wrangler.toml`
  still has the sample placeholder id. Paste the id from
  `wrangler d1 create shuttle` (or `wrangler d1 list`).
- **Bootstrap token rejected (401)** — an admin token has already been minted,
  which permanently consumes the bootstrap token. Use the persistent admin
  token; if it is lost, set a new `ADMIN_BOOTSTRAP_TOKEN` secret only after
  reviewing the grants in D1.
- **`node:` module resolution errors at runtime** — make sure
  `compatibility_flags = ["nodejs_compat"]` is present in `wrangler.toml`.
- **`stl sync` authentication failures** — verify `SHUTTLE_GATEWAY_TOKEN` is
  exported in the current shell, Access Service Auth is injected, and that the
  token's project matches the `--project` slug used at `stl sync init`.
- **OAuth requests return 401 from the Worker** — verify the Access application
  is using Managed OAuth, the account-member policy allows the user, and
  `ACCESS_TEAM_DOMAIN` plus `ACCESS_APPLICATION_AUD` match the Access app. The
  Worker requires a valid signed `Cf-Access-Jwt-Assertion`.
