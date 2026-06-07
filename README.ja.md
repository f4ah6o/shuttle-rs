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
`shuttle-gateway` を使います。gateway は request を設定済み repository に route し、
その repository 内で共通の `stl --json ...` executable を subprocess として実行します。
これにより、Shuttle storage は project-local のままになります。

`examples/projects.example.toml` から project config を作ります。project の `repo` path は
absolute path が必要です。任意の `db` path を指定する場合、それも absolute path にします。
`db` を省略すると、その repository の `.shuttle/shuttle.db` を使います。

config を指定して gateway を起動します。

```bash
shuttle-gateway serve --config projects.toml --addr 127.0.0.1:8787
```

`--addr` を省略すると、gateway は config の `[server].addr` を使います。gateway tool が
実行する CLI binary は `--stl /path/to/stl` で指定できます。subprocess timeout は
`--timeout <seconds>` で設定します。

MCP endpoint は 1 つだけ登録します。

```json
{
  "mcpServers": {
    "shuttle-gateway": {
      "url": "http://127.0.0.1:8787/mcp"
    }
  }
}
```

gateway では、`shuttle_remember` や `shuttle_task_create` のような write に明示的な
`project` argument が必要です。read は設定済み default project を使えます。
`SHUTTLE_GATEWAY_TOKEN` を設定すると、gateway boundary で
`Authorization: Bearer <token>` が必要になります。

gateway tool は解決した project repository 内で `stl --json ...` を実行します。そのため
identity、inbox ownership、`.shuttle/shuttle.db` は project-local のままです。web chat
client から write する場合は、明示的な `project` argument を渡してください。

ChatGPT や Claude web connector のような remote MCP client に gateway を公開する場合は、
gateway へ forward する public URL を OAuth config に書きます。

```toml
[oauth]
public_url = "https://shuttle.example.com"
admin_token_env = "SHUTTLE_OAUTH_ADMIN_TOKEN"
```

owner-approval token を runtime injection で渡して gateway を起動します。

```bash
SHUTTLE_OAUTH_ADMIN_TOKEN=<admin-token> \
shuttle-gateway serve --config projects.toml --addr 127.0.0.1:8787
```

remote MCP client には `https://shuttle.example.com/mcp` を登録します。OAuth client
registration、authorization code、access token は gateway-local SQLite database に保存されます。
`[oauth].db_path` に absolute path を指定しない場合、database は config file の隣の
`gateway-oauth.db` です。admin token は secret manager または runtime-injected
environment variable で渡してください。

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
