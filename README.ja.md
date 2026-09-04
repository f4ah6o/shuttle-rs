# shuttle-rs

[English](./README.md)

[![crates.io](https://img.shields.io/crates/v/shuttle-rs.svg)](https://crates.io/crates/shuttle-rs)
[![docs.rs](https://docs.rs/shuttle-rs/badge.svg)](https://docs.rs/shuttle-rs)
[![CI](https://github.com/f4ah6o/shuttle-rs/actions/workflows/ci.yaml/badge.svg)](https://github.com/f4ah6o/shuttle-rs/actions/workflows/ci.yaml)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](./LICENSE-MIT)

`shuttle-rs` は、コーディングエージェントが共有するメモリ、メッセージ、タスク、引き継ぎ、リポジトリ情報を追記専用のイベントログで扱うツールです。
ローカルモードでは、`stl` CLI が対象の Git リポジトリにある `.shuttle/shuttle.db` へイベントを保存します。
ローカルデータベースがなく、`SHUTTLE_GATEWAY_URL` が設定されている場合は、`SHUTTLE_GATEWAY_PROJECT` で選んだ Cloudflare Worker と D1 のプロジェクトを使います。
`shuttle-gateway` は複数のプロジェクトを一つの MCP エンドポイントから利用できるようにします。

## できること

- **メモリ**：メモ、決定、観察、パターン、事実、バグを種類付きで保存し、検索語とリポジトリ情報から順位付けして呼び出します。
- **リポジトリ情報**：ブランチ、コミット、変更状態、変更ファイルをイベントと関連付けて表示します。
- **エージェント間連携**：メッセージ、タスク、引き継ぎ、Codex と Claude Desktop の共同作業キューを同じイベントログで管理します。
- **ワークフロー**：リポジトリが所有する `shuttle.workflows.toml` を読み、実行状態、ステップの担当、チェックポイント、再調整を記録します。
- **MCP**：ローカル HTTP サーバー、OAuth 対応の公開エンドポイント、複数プロジェクト対応 gateway を提供します。
- **同期**：JSON アーカイブ、SQLite データベース、Cloudflare Worker と D1 を介してイベントを同期します。
- **アダプタールーティング**：リポジトリ情報とイベントログから決定的な埋め込みを作り、登録済み LoRA または PEFT アダプターを選択して実行用マニフェストを出力します。
- **JSON 出力**：`--json` を付けると、CLI の結果をエージェントやスクリプトから扱える形式で返します。

## インストール

Rust の stable toolchain と Git を用意して、crates.io からインストールします。

```bash
cargo install shuttle-rs --locked
```

このパッケージは `stl` と `shuttle-gateway` をインストールします。

```bash
stl version
shuttle-gateway version
```

ソースから実行する場合は、リポジトリを clone して Cargo を使います。

```bash
cargo build
cargo run --bin stl -- --help
cargo run --bin shuttle-gateway -- --help
```

## ローカルで使う

Git リポジトリのルートでストレージを初期化し、エージェント名を設定します。

```bash
stl init
stl identity set codex
```

メモリを保存し、タスクを作成して、リポジトリ全体の状態を読みます。

```bash
stl decide "SQLite をローカルイベントストアとして使う"
stl task create "MCP の認証設定を確認する"
stl context
```

`stl init` はローカルモード用の `.shuttle/shuttle.db` を作成します。
イベントログは追記専用で、メモリ、メッセージ、タスク、引き継ぎ、ワークフローの状態をイベントから投影します。

## クラウドファーストで使う

クラウドファーストモードでは、Cloudflare Worker と D1 をイベントの正本として使います。
`.shuttle/shuttle.db` がなく、`SHUTTLE_GATEWAY_URL` が設定されていると、このモードが選ばれます。
通常の `stl` コマンドを実行する前に、gateway URL、プロジェクト、プロジェクト単位のトークンを設定します。

```bash
export SHUTTLE_GATEWAY_URL=https://<gateway-host>
export SHUTTLE_GATEWAY_PROJECT=my-project
export SHUTTLE_GATEWAY_TOKEN=stl_...
stl context
```

クラウドファーストモードでは、`stl init` はローカルデータベースを作成せずエラーになります。
トークンを読む環境変数は、`.shuttle/remote.json` に別名がなければ `SHUTTLE_GATEWAY_TOKEN` です。

## メモリとリポジトリ情報

用途に応じた種類で情報を保存します。

```bash
stl remember "SQLite is the local event store"
stl decide "Use append-only events"
stl observe "The branch changed"
stl pattern "Project state is rebuilt from events"
stl fact "The database path is .shuttle/shuttle.db"
stl bug "Recall ranking needs inspection"
```

検索語と種類を指定して呼び出せます。

```bash
stl recall "SQLite"
stl recall "SQLite" --type decision
stl --json recall "SQLite"
```

リポジトリ単位またはブランチ単位の情報も取得できます。

```bash
stl context
stl context --repo
stl context --branch
stl --json context
```

## メッセージ、タスク、引き継ぎ

エージェント間の短い連絡にはメッセージを使います。

```bash
stl send claude "Please review the latest diff"
stl inbox
stl inbox --agent claude
stl inbox --watch
stl history
```

追跡する作業にはタスクを使います。

```bash
stl task create "Implement repository status"
stl task list
stl task claim <task-id>
stl task update <task-id> "Added tests"
stl task done <task-id>
```

作業主体を移す場合は引き継ぎを使います。

```bash
stl handoff request claude "Please continue this branch"
stl handoff list
stl handoff accept <handoff-id>
stl handoff done <handoff-id>
```

メッセージの内容は、残すべき状態へ昇格できます。

```bash
stl decide --from-message <message-id>
stl task create --from-message <message-id>
stl handoff request claude --from-message <message-id>
```

Codex Desktop と Claude Desktop の共同作業キューも操作できます。

```bash
stl collab start "Implement the checkout flow" --agents codex,claude
stl collab status
stl collab nudge claude "Please review the validation output"
stl collab pass claude <task-id> "Implementation is done; please review"
```

## リポジトリワークフロー

`shuttle.workflows.toml` は、リポジトリ内の仕様を参照してワークフローを定義します。
各ステップには `read_only`、`idempotent`、`non_idempotent` のいずれかを指定します。

```bash
stl workflow list
stl workflow show daily-triage
stl workflow start daily-triage
stl workflow step claim <run-id> read-spec
stl workflow step complete <run-id> read-spec --output '{"read":true}'
stl workflow status <run-id>
```

中断しただけのステップは、担当済みの状態を保ちます。
別のエージェントが引き受けるときは、理由とともに `--takeover` を指定して claim します。
担当済みの `non_idempotent` ステップを takeover すると、状態が `needs_reconcile` に変わります。

```bash
stl workflow step claim <run-id> <step-id> \
  --takeover \
  --reason "Previous agent was interrupted"
```

処理を繰り返さずに外部システムを確認し、観測した結果を記録します。

```bash
stl workflow reconcile <run-id> <step-id> --output '{"status":"completed"}'
```

## HTTP MCP サーバー

対象リポジトリで HTTP サーバーを起動します。

```bash
stl app serve --addr 127.0.0.1:8787
```

`/mcp` が MCP エンドポイントです。
`/` と `/api/*` は、受信箱、タスク、メモリ、リポジトリ情報を JSON で返します。

MCP クライアントには次の URL を設定します。

```json
{
  "mcpServers": {
    "shuttle": {
      "url": "http://127.0.0.1:8787/mcp"
    }
  }
}
```

Bearer 認証を使う場合は、サーバーを起動する前にトークンを環境変数へ設定します。

```bash
SHUTTLE_MCP_BEARER_TOKEN=<token> \
stl app serve --addr 127.0.0.1:8787
```

公開 URL と OAuth metadata を使う場合は、管理トークンと `--public-url` を指定します。

```bash
SHUTTLE_OAUTH_ADMIN_TOKEN=<admin-token> \
stl app serve \
  --addr 127.0.0.1:8787 \
  --public-url https://shuttle.example.com
```

Cloudflare Named Tunnel も `stl` から起動できます。

```bash
SHUTTLE_OAUTH_ADMIN_TOKEN=<admin-token> \
CLOUDFLARE_TUNNEL_TOKEN=<cloudflare-tunnel-token> \
stl app tunnel --public-url https://shuttle.example.com
```

## 複数プロジェクト gateway

`shuttle-gateway` は、認証とプロジェクト選択を一つの MCP サーバーにまとめます。
各プロジェクトは、リポジトリを直接開く `local` backend または `stl app serve` に接続する `http` backend として登録します。

設定例は [`examples/projects.example.toml`](./examples/projects.example.toml) にあります。

```toml
[defaults]
project = "main"

[[listeners]]
name = "public"
addr = "127.0.0.1:8787"
auth = "oauth"
public_url = "https://shuttle.example.com"
oauth_admin_token_env = "SHUTTLE_OAUTH_ADMIN_TOKEN"
# dynamic registration は default で無効。必要な場合だけ opt-in する:
# allow_dynamic_registration = true

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

設定ファイルを指定して起動します。

```bash
SHUTTLE_GATEWAY_TOKEN=<gateway-token> \
SHUTTLE_MAIN_BACKEND_TOKEN=<backend-token> \
shuttle-gateway serve --config projects.toml
```

書き込み系 MCP tool は `project` 引数を必要とします。
読み取り系 tool は `[defaults]` の project を使えます。

起動後の gateway には、HTTP API または MCP tool の `shuttle_project_add` でプロジェクトを追加できます。

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

Gateway の OCI image と LXC archive は GitHub Releases から配布されます。

OAuth client registration、authorization code、access token、refresh token は gateway-local
SQLite database に保存されます。
dynamic registration は default で無効で、redirect URI は exact な HTTPS match（loopback の
HTTP だけ例外）でなければなりません。
bearer value ではなく access token と refresh token の digest を保存します。
backend token と OAuth admin token は secret manager または runtime-injected environment
variable で渡してください。

authorization code を交換すると、有効期限 3600 秒の access token と refresh token を発行します。
`grant_type=refresh_token` に client_id と refresh_token を付けて POST /oauth/token を呼ぶと、
新しい access token と新しい refresh token を取得できます。
refresh token は使用のたびに rotate し、有効期限は rotate 時点から 30 日先へ更新されます。
rotate 前の access token は自身の有効期限まで使えます。

消費済みの refresh token を再提示した場合、消費から 30 秒以内であれば `invalid_grant` を返す
だけで、同時実行や client の再送で接続が切れることはありません。
30 秒を過ぎてからの再提示は盗用とみなし、その authorization から派生した refresh token と
access token をすべて revoke します。

POST /oauth/revoke は token を revoke します。
refresh token を渡すと同じ authorization に属する access token も含めて revoke し、access
token を渡すとその access token だけを revoke します。

OCI image は GHCR から取得できます。

```bash
docker pull ghcr.io/f4ah6o/shuttle-gateway:<version>
```

## イベントの同期

### Cloudflare gateway

Cloudflare Worker と D1 を使う gateway の構築手順は [`docs/deploy-cloudflare.md`](./docs/deploy-cloudflare.md) にあります。

接続情報をリポジトリへ保存します。
トークン値は保存せず、トークンを読む環境変数名だけを記録します。

```bash
export SHUTTLE_GATEWAY_TOKEN=stl_...
stl sync init --url https://<gateway-host> --project my-project
```

イベントを送受信します。

```bash
stl sync push
stl sync pull
stl sync
```

`stl sync` は push の後に pull を実行します。
イベント ID を保つため、同じイベントを再送しても重複しません。

### ファイルと SQLite

JSON archive を介してイベントを移動できます。

```bash
stl mesh export shuttle-events.json
stl mesh import shuttle-events.json
```

別の Shuttle database と直接同期することもできます。

```bash
stl mesh sync /path/to/peer/.shuttle/shuttle.db
```

同期先では、取り込んだイベントがメモリ、メッセージ、タスク、引き継ぎとして表示されます。

## アダプタールーティング

Shuttle は、リポジトリ構造、Git metadata、イベントログからプロジェクト埋め込みを作ります。
ローカル registry に登録したアダプターとの cosine similarity を計算し、選択結果、merge plan、外部推論エンジン向け manifest を出力します。
Shuttle 自体はモデル推論を実行しません。

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

`doc2lora` は、リポジトリ情報とイベントログから `context.md` を生成し、外部 runner にアダプター生成を依頼します。
runner が返した manifest を使って、生成物を registry へ登録します。

```bash
stl adapter doc2lora \
  --name project-lora \
  --base-model Qwen/Qwen2.5-Coder-7B-Instruct \
  --out-dir ./adapters/project-lora \
  --tag generated \
  --focus "adapter routing"
```

runner は `--runner`、`SHUTTLE_DOC2LORA_RUNNER`、`PATH` 上の `doc2lora` の順に解決されます。

## コーディングエージェントとの連携

Codex または Claude Code 用の skill を出力し、各ツールの設定先へインストールできます。

```bash
stl skill print codex
stl skill install codex
stl skill print claude
stl skill install claude
```

リポジトリ内の標準手順は [`AGENTS.md`](./AGENTS.md) にあります。

ツール別の設定は次の文書にあります。

- [Codex](./docs/codex.md)
- [Claude Code](./docs/claude-code.md)
- [OpenCode](./docs/opencode.md)
- [Codex Desktop と Claude Desktop の共同作業](./docs/desktop-collaboration.md)

## Telemetry

診断ログは stderr に出力し、通常出力と JSON 出力を汚しません。
ログレベルは `RUST_LOG` で指定します。

```bash
RUST_LOG=info,shuttle_rs=debug stl context
```

OpenTelemetry の OTLP export は環境変数で有効にします。

```bash
SHUTTLE_OTEL=1 \
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317 \
OTEL_SERVICE_NAME=stl \
RUST_LOG=info,shuttle_rs=debug \
stl app serve --addr 127.0.0.1:8787
```

trace attribute にはコマンド名や request metadata を記録します。
メモリ本文、メッセージ本文、OAuth token、Bearer token、request body は記録しません。

## 開発

CI と同じ検証をローカルで実行します。

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
```

release 前の確認には `just release-check` を使えます。

## Machine-readable contract

JSON CLI output と structured MCP result は schema `shuttle.v1` を識別します。
collection は `schema_version`、`items`、`pagination` を持つ object で返し、error は安定した
`code`、`message`、`retryable` を持ちます。schema と fixture は `schemas/v1` にあります。
成功レスポンスの field は additive に進化させ、breaking change では schema version を更新します。
diagnostic と log は stderr に出力し、JSON stdout を汚染しません。

## Acknowledgements

Shuttle は、コーディングエージェント間でメモリを共有する [kioku-mesh](https://github.com/h-wata/kioku-mesh) から着想を得ています。

タスク調整の設計には [rally-rs](https://github.com/f4ah6o/rally-rs) と [agmsg](https://github.com/fujibee/agmsg) の考え方を取り入れています。

## License

MIT License または Apache License 2.0 の条件で利用できます。
