# shuttle-gateway Worker を Cloudflare にデプロイする

[English](./deploy-cloudflare.md)

このガイドでは、cloud shuttle-gateway
([`workers/shuttle-gateway/`](../workers/shuttle-gateway/) の Cloudflare
Worker)を会社の Cloudflare アカウントへ、既存の `obr-grp.com` ゾーンと
Cloudflare Access を使ってデプロイする手順を説明します。

この Worker は、Cloudflare D1 を永続的な共有 event store として使う、
stateless な MCP endpoint とリソース指向の HTTP API です。Rust 製の
`shuttle-gateway` バイナリとは独立しています。セルフホストする Rust 版
gateway については、リポジトリ [README](../README.ja.md) の
「Multi-project Gateway」節と `packaging/` ディレクトリを参照してください。

## 前提条件

- Workers と D1 が有効な会社の Cloudflare アカウント。
- 同じアカウントで管理している `obr-grp.com` ゾーン。
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

- `PUBLIC_URL` — デプロイされた Worker の base URL。今回の値は
  `https://shuttle.obr-grp.com` です。MCP/OAuth metadata に反映されます。
- `ADMIN_OWNER_ID` — 共有 tenant の固定 owner id。今回の値は `obr-grp` です。
- `ACCESS_TEAM_DOMAIN` — Cloudflare Access team の issuer URL。例:
  `https://<team-name>.cloudflareaccess.com`。
- `ACCESS_APPLICATION_AUD` — この hostname を保護する Access application の
  Application Audience (AUD) tag。

チェックイン済みの設定は `workers_dev = false` と custom domain
`shuttle.obr-grp.com` を使います。`workers.dev` の公開 endpoint は有効に
しないでください。

最初の実クライアント接続前に、Cloudflare Access で
`shuttle.obr-grp.com/*` の Self-hosted application を作成し、次の allow policy
を設定します:

1. 人間の MCP client 用に Cloudflare account member 全員を許可する policy。
2. terminal と agent 用に Service Auth service token を許可する policy。

Access のログイン画面に「Send login code」(One-time PIN)しか表示されない
場合は、Allow policy の Include に対象メールアドレスまたはメールドメイン
も追加してください。`cloudflare_account_member` selector は Cloudflare
identity provider で評価されるため、それだけでは OTP login を許可しません。

Application の Advanced settings で Managed OAuth を有効にします。Access の
team domain と application の AUD tag を `wrangler.toml` の
`ACCESS_TEAM_DOMAIN` と `ACCESS_APPLICATION_AUD` に設定してください。Worker
自身も `Cf-Access-Jwt-Assertion` の署名・issuer・audience を検証します。
header だけを信頼しないでください。`/api/health` も保護対象に含めます。
health は公開 liveness endpoint ではありません。

ChatGPT web の MCP client を接続する場合は、Managed OAuth の
「Allowed redirect URIs」に次の URI パターンを追加してください:

```text
https://chatgpt.com/connector/oauth/*
```

ChatGPT は `/connector/oauth/` 以下に callback ID を付けて使用します。
`chatgpt.com` 全体を許可せず、このパスだけを許可してください。古い
ChatGPT connector のため、次の固定URIも残します:

```text
https://chatgpt.com/connector_platform_oauth_redirect
```

許可されていない URI のままだと Dynamic Client Registration が拒否されます。

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
admin token を安全に保存したあと、bootstrap secret は削除してください。

## 5. デプロイする

```bash
npm run deploy
```

Worker の health を確認します。Access の Service Auth header と PAT の両方が
必要です:

```bash
curl -s https://shuttle.obr-grp.com/api/health \
  -H "CF-Access-Client-Id: $CLOUDFLARE_ACCESS_CLIENT_ID" \
  -H "CF-Access-Client-Secret: $CLOUDFLARE_ACCESS_CLIENT_SECRET" \
  -H "authorization: Bearer $SHUTTLE_GATEWAY_TOKEN"
curl -s https://shuttle.obr-grp.com/mcp \
  -H "CF-Access-Client-Id: $CLOUDFLARE_ACCESS_CLIENT_ID" \
  -H "CF-Access-Client-Secret: $CLOUDFLARE_ACCESS_CLIENT_SECRET" \
  -H "authorization: Bearer $SHUTTLE_GATEWAY_TOKEN"        # MCP health
```

## 6. token と project を bootstrap する

bootstrap token をちょうど一度だけ使い、永続的な admin token を発行します。
admin token の発行によって bootstrap token は消費されるため、レスポンスを
必ず保存してください。

```bash
URL=https://shuttle.obr-grp.com
ACCESS_HEADERS=(
  -H "CF-Access-Client-Id: $CLOUDFLARE_ACCESS_CLIENT_ID"
  -H "CF-Access-Client-Secret: $CLOUDFLARE_ACCESS_CLIENT_SECRET"
)

# 1. bootstrap token で永続 admin token を発行する(one-time)。
curl -sX POST "$URL/api/tokens" "${ACCESS_HEADERS[@]}" -H "authorization: Bearer $BOOTSTRAP" \
  -H 'content-type: application/json' -d '{"scopes":["admin"]}'
# -> { "token": "stl_...", "scopes": ["admin"], ... }   これを保存する

# 2. admin token で project を作成する。
curl -sX POST "$URL/api/projects" "${ACCESS_HEADERS[@]}" -H "authorization: Bearer $ADMIN" \
  -H 'content-type: application/json' -d '{"slug":"taskforward"}'

# 3. ローカル agent 用に project スコープの token を発行する。
curl -sX POST "$URL/api/tokens" "${ACCESS_HEADERS[@]}" -H "authorization: Bearer $ADMIN" \
  -H 'content-type: application/json' \
  -d '{"project":"taskforward","scopes":["read","write"],"agent_id":"linux-codex","client_instance_id":"linux-codex"}'
```

token は `read`/`write`/`admin` scope の personal access token で、サーバー
側には SHA-256 hash のみが保存されます。紛失した token は復元できないため、
再発行してください。

## 7. ローカルリポジトリを接続する(移行時のみ)

`stl sync` は既存の repo-local `.shuttle` を D1 へ移行する間、および
local-mode リポジトリとの互換性のために残っています。移行対象では、手順 6
の project スコープ token を使って次を実行します:

```bash
export SHUTTLE_GATEWAY_TOKEN=stl_...   # gateway が発行した scoped PAT
stl sync init --url https://shuttle.obr-grp.com --project taskforward
stl sync push   # ローカル event をアップロード(event id で冪等)
stl sync pull   # gateway の event をこの workspace にダウンロード
stl sync        # 両方: push してから pull
```

`stl sync init` は `.shuttle/remote.json`(URL、project、`--token-env` 指定
時は token の環境変数名)を書き出します。token 自体は保存されません。
`taskforward` では D1 の件数・内容・動作を確認して local `.shuttle` を削除
した後、`SHUTTLE_GATEWAY_URL`、`SHUTTLE_GATEWAY_PROJECT`、
`SHUTTLE_GATEWAY_TOKEN`、`SHUTTLE_CLIENT_INSTANCE_ID` が設定されていれば
`stl` は自動的に cloud-first mode になります。offline の local fallback は
作りません。freeze、Box backup、件数検証、削除の順序は
[taskforward の cloud 仕様](../../taskforward/docs/spec/shuttle-cloud.md)を
参照してください。

headless MCP client と local repository は、Cloudflare Access Service Auth
header と `Authorization: Bearer stl_...` header を付けて
`https://shuttle.obr-grp.com/mcp` に接続します。ChatGPT や Claude.ai のような
interactive MCP client は Cloudflare Access Managed OAuth を discovery して
接続し、Shuttle PAT は受け取りません。OAuth user は共有 `obr-grp` tenant の
read/write を使い、admin 操作は PAT のみです。

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
  `SHUTTLE_GATEWAY_TOKEN` と Cloudflare Access Service Auth が注入されて
  いること、token の project が `stl sync init` の `--project` slug と一致
  していることを確認してください。
- **OAuth request が Worker から 401 になる** — Access application で Managed
  OAuth が有効で、account-member policy が user を許可していること、
  `ACCESS_TEAM_DOMAIN` と `ACCESS_APPLICATION_AUD` が Access application と
  一致していることを確認してください。Worker は署名済みの
  `Cf-Access-Jwt-Assertion` を要求します。
