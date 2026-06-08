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

[English](./README.md)

`shuttle-rs` は、agent の memory、message、repository context、coordination
を扱う local-first な event log です。`stl` CLI は、現在の Git repository
直下の `.shuttle/shuttle.db` にデータを保存します。

## Agent Onboarding

coding agent 向けの標準 workflow は [AGENTS.md](./AGENTS.md) にあります。
tool 別の setup は [opencode](./docs/opencode.md)、[Claude Code](./docs/claude-code.md)、
[Codex](./docs/codex.md) を参照してください。Claude Code では、慣例的な入口として
[CLAUDE.md](./CLAUDE.md) も使えます。

Shuttle の Codex skill を install します。

```bash
stl skill install codex
```

書き込まずに生成される skill を確認するには、次を実行します。

```bash
stl skill print codex
```

## Phase 1 Commands

local storage を初期化します。

```bash
stl init
```

local agent identity を確認または設定します。

```bash
stl identity current
stl identity set codex
```

`SHUTTLE_AGENT` は `.shuttle/agent` の repo-local identity より優先されます。
どちらも未設定の場合、Shuttle は `unknown` を使います。

generic memory と typed memory を記録します。

```bash
stl remember "SQLite is the local event store"
stl decide "SQLite remains the default because it is local-first"
stl observe "The current branch changes recall ranking"
stl pattern "Use event projections rather than separate state tables"
stl fact "The event log is append-only"
stl bug "Recall ordering should prefer same-repo decisions"
```

ranked results として memory を検索します。

```bash
stl recall "SQLite decision"
stl recall "SQLite decision" --type decision
stl --json recall "SQLite decision"
```

repository context を読みます。

```bash
stl context
stl context --repo
stl context --branch
stl --json context
```

agent message を送受信します。

```bash
stl send claude "Please review the latest diff"
stl send --from claude codex "LGTM after the last fix"
stl inbox
stl inbox --agent claude
stl inbox --watch
stl history
```

message は memory、task、handoff、repository context と同じ append-only event log
に保存されます。message は一時的な agent 間 communication、task は追跡する作業、
handoff は ownership transfer、typed memory は残すべき outcome に使います。

task と handoff を操作します。

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

残すべき message は durable project state に昇格できます。

```bash
stl decide --from-message <message-id>
stl task create --from-message <message-id>
stl handoff request claude --from-message <message-id>
```

HTTP MCP として Shuttle を公開します。

```bash
stl app serve --addr 127.0.0.1:8787
```

app は `/` に小さな JSON dashboard を出し、dashboard state、inbox、tasks、
memories、repository context 用の API endpoint も提供します。MCP endpoint は
`/mcp` です。

MCP client には `/mcp` endpoint を設定します。

```json
{
  "mcpServers": {
    "shuttle": {
      "url": "http://127.0.0.1:8787/mcp"
    }
  }
}
```

`stl app serve` の起動前に `SHUTTLE_MCP_BEARER_TOKEN` を設定すると、MCP request
に `Authorization: Bearer <token>` が必要になります。未設定の場合、local MCP は
認証なしで動きます。

すでに public HTTPS endpoint の後ろで app を動かしていて OAuth metadata を公開する
場合は、`--public-url` を渡します。

```bash
SHUTTLE_OAUTH_ADMIN_TOKEN=<admin-token> \
stl app serve --addr 127.0.0.1:8787 --public-url https://shuttle.example.com
```

Cloudflare Named Tunnel で、web chat client 向けの remote MCP server として Shuttle
を公開できます。

```bash
SHUTTLE_OAUTH_ADMIN_TOKEN=<admin-token> \
CLOUDFLARE_TUNNEL_TOKEN=<cloudflare-tunnel-token> \
stl app tunnel --public-url https://shuttle.example.com
```

ChatGPT や Claude には `https://shuttle.example.com/mcp` を設定します。tunnel token
は環境変数からだけ読みます。shell history に残さず、secret manager または runtime
injection で渡してください。public URL は、`http://127.0.0.1:8787` に forward する
Cloudflare Tunnel hostname と一致している必要があります。

MCP server は memory、message、task、handoff、repository context、repository status、
changed-file、diff の tool を提供します。`remember` のような alias と
`shuttle_memory_store` のような namespaced tool は、同じ local event log を呼びます。

## Multi-project Gateway

複数の local repository を 1 つの MCP server から使う web chat client には、
`shuttle-gateway` を使います。gateway は MCP、auth、project routing の境界です。
各 project は local backend として `stl --json ...` を実行するか、repo-local な
`stl app serve` に HTTP で接続できます。

remote deployment model は次の形です。

```text
gateway host / LXC
└─ shuttle-gateway

project environment
└─ stl app serve
   └─ repo + .shuttle/shuttle.db
```

`examples/projects.example.toml` から project config を作ります。remote project
environment には `backend = "http"` と `url` を使います。compatibility mode では
`backend = "local"` と absolute な `repo` path を指定します。`backend` を省略して
`repo` がある場合、その project は local として扱われます。

config を指定して gateway を起動します。

```bash
shuttle-gateway serve --config projects.toml
```

installed binary の version を確認します。

```bash
stl version
shuttle-gateway version
stl --json version
```

local backend が実行する CLI binary は `--stl /path/to/stl` で指定できます。
local subprocess と HTTP backend の timeout は `--timeout <seconds>` で設定します。
single-listener config では `--addr` で address を上書きできます。複数の
`[[listeners]]` を持つ config では、listener address は config 側が管理します。

listener は ingress/auth policy を project backend type から分離します。public Web Chat
listener には OAuth を使い、private LAN/Tailscale/local listener には bearer auth または
loopback-only の認証なし access を使えます。

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

gateway では、`shuttle_remember` や `shuttle_task_create` のような write に明示的な
`project` argument が必要です。read は設定済み default project を使えます。

HTTP backend では、project environment で repo-local app server を起動します。

```bash
SHUTTLE_MCP_BEARER_TOKEN=<backend-token> \
stl app serve --addr 127.0.0.1:8787
```

gateway project には URL と backend token の environment variable 名を設定します。

```toml
[projects.main]
backend = "http"
url = "http://10.10.10.21:8787"
token_env = "SHUTTLE_MAIN_BACKEND_TOKEN"
```

ChatGPT や Claude web connector のような remote MCP client には public listener URL を
登録します。

```json
{
  "mcpServers": {
    "shuttle-gateway": {
      "url": "https://shuttle.example.com/mcp"
    }
  }
}
```

OAuth listener は owner-approval token を runtime injection で渡して起動します。

```bash
SHUTTLE_OAUTH_ADMIN_TOKEN=<admin-token> \
shuttle-gateway serve --config projects.toml
```

OAuth client registration、authorization code、access token は gateway-local SQLite database
に保存されます。backend token と OAuth admin token は secret manager または
runtime-injected environment variable で渡してください。

LXC-oriented な gateway host 向けには、`v*` release tag で
`shuttle-gateway-lxc-<target>.tar.gz` archive を公開します。archive には
`bin/shuttle-gateway`、`bin/stl`、LXC config example、systemd unit、`install.sh`
が含まれます。installer は default で `/usr/local/bin`、`/etc/shuttle-gateway`、
`/var/lib/shuttle-gateway` を使います。install 後に `projects.toml` と
`shuttle-gateway.env` を編集してください。example file に実 token value を保存しないでください。

同じ `v*` release で OCI image と architecture 別の OCI layout archive も公開します。
registry image は GHCR から pull できます。

```bash
docker pull ghcr.io/f4ah6o/shuttle-gateway:<version>
```

config directory と state directory を mount して起動します。

```bash
docker run --rm \
  -p 8787:8787 \
  -v /path/to/shuttle-gateway:/etc/shuttle-gateway \
  -v /path/to/shuttle-gateway-state:/var/lib/shuttle-gateway \
  -e SHUTTLE_OAUTH_ADMIN_TOKEN=<admin-token> \
  -e SHUTTLE_MAIN_BACKEND_TOKEN=<backend-token> \
  ghcr.io/f4ah6o/shuttle-gateway:<version>
```

Apple の `container` tool でも同じ OCI image を pull するか、release archive を
load できます。

```bash
container image pull ghcr.io/f4ah6o/shuttle-gateway:<version>
container image load --input shuttle-gateway-oci-linux-arm64.tar
container run \
  -p 8787:8787 \
  -v /path/to/shuttle-gateway:/etc/shuttle-gateway \
  -v /path/to/shuttle-gateway-state:/var/lib/shuttle-gateway \
  ghcr.io/f4ah6o/shuttle-gateway:<version>
```

Shuttle instance 間で event log を同期します。

```bash
stl mesh export shuttle-events.json
stl mesh import shuttle-events.json
stl mesh sync /path/to/peer/.shuttle/shuttle.db
```

Git repository 内で command を実行すると、Shuttle は captured event に repository metadata
を付けます。metadata には repository path、remote から導いた repository id、branch、commit、
dirty status、dirty file names が含まれます。

task と handoff の state は append-only event から projection されます。別の task table は
不要です。JSON output は MCP client から扱える形を保ちます。

mesh synchronization は stable event id で import し、duplicate を skip します。そのため、
offline のあとに同じ sync を再実行しても、相手の store がまだ見ていない event だけを転送します。
CLI は imported event を受信側 workspace id に normalize し、source workspace を event metadata
に記録します。sync された task、handoff、message、memory は local command から見えます。

## Acknowledgements

Shuttle は [kioku-mesh](https://github.com/h-wata/kioku-mesh) に影響を受けています。
kioku-mesh は tool や machine をまたいで AI coding agent が使う shared memory system です。

Shuttle は [rally-rs](https://github.com/f4ah6o/rally-rs) の ideas も取り入れています。
rally-rs は [agmsg](https://github.com/fujibee/agmsg) をもとにしています。
