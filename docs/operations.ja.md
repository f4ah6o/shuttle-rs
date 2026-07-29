# Shuttle の運用

local app と Rust gateway は process の liveness 用 /healthz と、traffic readiness 用
/readyz を公開します。どちらも MCP 認証を要求せず、project 一覧や configuration の
値を返しません。

readiness は local SQLite の integrity と migration 状態、設定された認証 storage を
確認します。required な bearer token が無い gateway listener は unready になります。
optional backend の外部 probe で startup を無期限に待つことはありません。

両 server は Ctrl-C と SIGTERM を受けると新しい connection の受付を止め、in-flight
request を drain します。drain の上限は SHUTTLE_SHUTDOWN_TIMEOUT_SECS で設定でき、
default は 30 秒です。request と backend operation の trace metadata は bounded にし、
bearer token、authorization header、event content、repository path、OAuth code、
verifier value は span field に記録しません。

local database には stl db status、stl db check、stl db backup <path> を使います。
backup は既存の destination を上書きしません。
