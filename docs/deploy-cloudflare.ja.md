# shuttle-gateway Worker を Cloudflare にデプロイする

[English](./deploy-cloudflare.md)

このガイドでは、cloud shuttle-gateway
([`workers/shuttle-gateway/`](../workers/shuttle-gateway/) の Cloudflare
Worker)を、新規の Cloudflare アカウントから、project・token の作成、ローカル
リポジトリの同期までを通してデプロイする手順を説明します。

この Worker は、Cloudflare D1 を永続的な共有 event store として使う、
stateless な MCP endpoint とリソース指向の HTTP API です。Rust 製の
`shuttle-gateway` バイナリとは独立しています。セルフホストする Rust 版
gateway については、リポジトリ [README](../README.ja.md) の
「Multi-project Gateway」節と `packaging/` ディレクトリを参照してください。

## 前提条件

- Workers と D1 が有効な Cloudflare アカウント(開始時点では free tier で
  十分です)。
- Node.js 20 以上と npm。
- ローカルにチェックアウトしたリポジトリ。

以下のコマンドはすべて Worker ディレクトリで実行します:

```bash
cd workers/shuttle-gateway
npm install
```

wrangler を Cloudflare アカウントで認証します(ブラウザが開きます):

```bash
npx wrangler login
```

非対話環境(CI)では、ログインの代わりに `CLOUDFLARE_API_TOKEN`(token が
複数アカウントにまたがる場合は `CLOUDFLARE_ACCOUNT_ID` も)を設定します。
token は secret manager や実行時注入で扱い、コミットしないでください。

## 1. D1 database を作成する

```bash
npx wrangler d1 create shuttle
```

コマンドが `database_id` を出力します。
[`wrangler.toml`](../workers/shuttle-gateway/wrangler.toml) の placeholder を
置き換えます:

```toml
[[d1_databases]]
binding = "DB"
database_name = "shuttle"
database_id = "<wrangler d1 create が出力した id>"
migrations_dir = "migrations"
```

## 2. vars を設定する

引き続き `wrangler.toml` で:

- `PUBLIC_URL` — デプロイされた Worker の公開 base URL(例:
  `https://shuttle-gateway.<your-subdomain>.workers.dev`、または custom
  domain)。MCP/OAuth metadata に反映されます。
- `ADMIN_OWNER_ID` — bootstrap admin token に紐づく owner id。デフォルトの
  `owner-local` のままで問題ありません。最初の scoped token の発行と最初の
  project 作成にのみ使われます。

`workers.dev` の URL がまだ分からない場合は、一度デプロイ(手順 5)して
wrangler が出力する URL を控え、`PUBLIC_URL` を設定して再デプロイして
ください。

## 3. migration を適用する

```bash
npm run migrate:remote
```

`migrations/0001_init.sql`(および以降の migration)がリモートの D1 database
に適用されます。migration を追加する変更を pull したあとは再実行して
ください。適用は migration ごとに冪等です。

## 4. bootstrap admin secret を設定する

強度の高い one-time token を生成し、Worker の secret として保存します:

```bash
openssl rand -hex 32   # 値を生成し、一時的に安全な場所に保管する
npx wrangler secret put ADMIN_BOOTSTRAP_TOKEN
```

`wrangler secret put` は値を stdin で受け取るため、token が shell history や
リポジトリに残ることはありません。

bootstrap token は文字どおり one-time です。最初の `admin` scope の token が
発行されるまでのみ有効で、以降は恒久的に拒否されます。

## 5. デプロイする

```bash
npm run deploy
```

wrangler がデプロイ先 URL を出力します。Worker の health を確認します:

```bash
curl -s https://<gateway-host>/api/health
curl -s https://<gateway-host>/mcp        # MCP health
```

## 6. token と project を bootstrap する

bootstrap token をちょうど一度だけ使い、永続的な admin token を発行します。
admin token の発行によって bootstrap token は消費されるため、レスポンスを
必ず保存してください。

```bash
URL=https://<gateway-host>

# 1. bootstrap token で永続 admin token を発行する(one-time)。
curl -sX POST "$URL/api/tokens" -H "authorization: Bearer $BOOTSTRAP" \
  -H 'content-type: application/json' -d '{"scopes":["admin"]}'
# -> { "token": "stl_...", "scopes": ["admin"], ... }   これを保存する

# 2. admin token で project を作成する。
curl -sX POST "$URL/api/projects" -H "authorization: Bearer $ADMIN" \
  -H 'content-type: application/json' -d '{"slug":"my-project"}'

# 3. ローカル agent 用に project スコープの token を発行する。
curl -sX POST "$URL/api/tokens" -H "authorization: Bearer $ADMIN" \
  -H 'content-type: application/json' \
  -d '{"project":"my-project","scopes":["read","write"]}'
```

token は `read`/`write`/`admin` scope の personal access token で、サーバー
側には SHA-256 hash のみが保存されます。紛失した token は復元できないため、
再発行してください。

## 7. ローカルリポジトリを接続する

gateway と同期する各リポジトリで、手順 6 の project スコープ token を使って
`stl sync` を設定します:

```bash
export SHUTTLE_GATEWAY_TOKEN=stl_...   # gateway が発行した scoped PAT
stl sync init --url https://<gateway-host> --project my-project
stl sync push   # ローカル event をアップロード(event id で冪等)
stl sync pull   # gateway の event をこの workspace にダウンロード
stl sync        # 両方: push してから pull
```

`stl sync init` は `.shuttle/remote.json`(URL、project、`--token-env` 指定
時は token の環境変数名)を書き出します。token 自体は保存されません。sync の
詳細な仕様は [AGENTS.md](../AGENTS.md) の「Cloud Sync (Cloudflare gateway)」
を参照してください。

Streamable HTTP を話せる MCP client は、`Authorization: Bearer stl_...`
header 付きで `https://<gateway-host>/mcp` に接続できます。ChatGPT/Claude.ai
の web client 向け OAuth 2.1 は未実装(issue #46 で追跡中)のため、現時点では
PAT ベースの client のみ対応です。

## ローカル開発

ローカルの D1 エミュレーションに対して Worker を実行します:

```bash
npm run migrate:local
npm run dev
```

`npm test` は、D1 adapter と同じ storage port を実装した Node 組み込みの
SQLite に対してテストを実行します。`npm run typecheck` で Worker を型検査
できます。

## デプロイの更新

```bash
git pull
npm install
npm run migrate:remote   # 新しい migration のみ適用される
npm run deploy
```

デプロイは Cloudflare 側で atomic に行われ、D1 のデータ・secret・発行済み
token は再デプロイ後も維持されます。

## トラブルシューティング

- **deploy や migrate で `database_id` エラーになる** — `wrangler.toml` が
  全ゼロの placeholder id のままです。`wrangler d1 create shuttle`(または
  `wrangler d1 list`)の id を貼り付けてください。
- **bootstrap token が拒否される(401)** — すでに admin token が発行され、
  bootstrap token は恒久的に消費されています。永続 admin token を使って
  ください。それも紛失した場合は、D1 の grant を確認したうえで新しい
  `ADMIN_BOOTSTRAP_TOKEN` secret を設定してください。
- **実行時に `node:` モジュール解決エラーになる** — `wrangler.toml` に
  `compatibility_flags = ["nodejs_compat"]` があることを確認してください。
- **`stl sync` の認証が失敗する** — 現在の shell で
  `SHUTTLE_GATEWAY_TOKEN` が export されていること、token の project が
  `stl sync init` の `--project` slug と一致していることを確認してください。
