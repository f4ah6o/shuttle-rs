# Rust依存グラフを縮小する

Status: open
Model: GPT-5.6 Sol
Created: 2026-08-12
Updated: 2026-08-12
Branch: docs/20260812-dependency-reduction
Priority: P1

## 概要

`shuttle-rs` のRust依存を、CLI、gateway、telemetry、HTTP/MCPなどの機能境界に沿って整理し、通常の `stl` 利用で不要な重い依存をビルドしない構成へ移す。

既にworkspace内では `stl`、`shuttle-core`、`shuttle-mcp`、`shuttle-store` など複数crateへ分割されているため、その境界を活用してroot packageが広い依存を再集約しない構成を目指す。

## 背景

root `Cargo.toml` は `stl` と `shuttle-gateway` のbinを同じpackageで提供し、`axum`、`tokio`、`ureq`、`rusqlite`、OpenTelemetry、OTLP、`tower-http`、`tracing-opentelemetry` などをまとめて直接依存している。

特にOTLPのgRPC exporterは通常のlocal-first CLI操作に必須ではない場合、依存グラフとcompile timeを大きく増やす。
またworkspace側では機能crateが既に分離されているため、binaryごとの依存境界を明確にすれば削減余地がある。

## 目標

- `stl` の通常利用に不要なtelemetry / gateway依存を外せるようにする。
- root packageとworkspace crateの責務重複を整理する。
- HTTP / MCP / gateway機能の依存を必要なbinaryまたはcrateに閉じ込める。
- local-first CLI、SQLite保存、MCP、mesh、cloud syncの既存挙動を維持する。
- 変更前後の依存package数、compile time、binary sizeを記録する。

## 提案する方針

### 1. telemetryをoptional化する

以下を含むtelemetry stackの使用箇所を確認し、通常のCLI実行に必須でなければ `telemetry` featureへ分離する。

- `opentelemetry`
- `opentelemetry-otlp`
- `opentelemetry_sdk`
- `tracing-opentelemetry`
- telemetry専用の `tracing-subscriber` feature

`grpc-tonic` exporterを有効にするのはtelemetry feature有効時だけにする。

### 2. stlとgatewayのpackage境界を整理する

workspaceに存在する `crates/stl` と各機能crateを基準に、root packageが `stl` と `shuttle-gateway` 双方の全依存を抱える必要があるか確認する。

必要であれば次の形へ寄せる。

- `crates/stl`: CLIに必要な依存だけ
- gateway package / crate: HTTP、server、telemetryなどgateway固有依存
- shared crates: domain / store / protocolの共通依存だけ

大規模なrenameは避け、既存binary名と公開CLI互換性を維持する。

### 3. HTTP client/server stackを局所化する

`axum`、`tower-http`、`ureq`、`tokio` の使用箇所を確認し、server専用、remote専用、CLI専用の依存を対応crateへ移す。

### 4. default featureと重複versionを整理する

`cargo tree --edges features` と `cargo tree -d` を使い、不要なdefault featureと重複versionを確認する。
`rusqlite`、`uuid`、`chrono` などworkspace共通crateは、必要featureをworkspaceレベルで揃える。

### 5. 依存回帰を計測する

最低限次のbefore/afterを記録する。

- `stl` のdependency package数
- `shuttle-gateway` のdependency package数
- `cargo tree -d`
- `cargo build --release --bin stl` のclean build時間
- `cargo build --release --bin shuttle-gateway` のclean build時間
- 両binaryのrelease size

## 受け入れ条件

- [ ] telemetry無効時の `stl` dependency graphにOTLP/gRPC exporter専用crateが含まれない。
- [ ] telemetry有効時は既存のtrace/export挙動が維持される。
- [ ] `stl` とgateway固有dependencyが可能な範囲でpackage/crate境界に分離される。
- [ ] root packageがworkspace crateの依存を不必要に再集約しない。
- [ ] local SQLite mode、cloud-first mode、MCP、mesh、workflowの既存CLI契約が維持される。
- [ ] `cargo fmt --check` が成功する。
- [ ] `cargo test --workspace` が成功する。
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` が成功する。
- [ ] 変更前後のpackage数、compile time、binary sizeが記録される。

## 対象外

- CLI command名や保存schemaの変更
- Cloudflare protocolの再設計
- observability機能そのものの削除
- SQLiteから別DBへの移行
- dependency version更新だけを目的とした作業

## 実装順

1. `stl` / gateway別のdependency baselineを取得する。
2. telemetryをoptional featureへ分離する。
3. binary固有dependencyを対応crate/packageへ移す。
4. default featureと重複versionを整理する。
5. workspace全体のテストを実行する。
6. before/afterを記録する。
