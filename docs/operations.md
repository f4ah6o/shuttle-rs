# Shuttle operations

The local app and Rust gateway expose /healthz for process liveness and
/readyz for traffic readiness. Neither endpoint requires MCP authentication,
enumerates projects, or returns configuration values.

Readiness checks the local SQLite integrity/migration state and configured
authentication storage. A missing required bearer token makes a gateway
listener unready; an optional remote project is not probed during startup.

Both servers handle Ctrl-C and SIGTERM, stop accepting new connections, and
drain in-flight requests. Set SHUTTLE_SHUTDOWN_TIMEOUT_SECS to bound the drain
window (default 30 seconds). Requests and backend operations are traced with
bounded metadata; bearer tokens, authorization headers, event contents,
repository paths, OAuth codes, refresh tokens, and verifier values are never
span fields.

When a consumed OAuth refresh token is presented again after the grace window,
the server revokes every token derived from that authorization and emits one
warn-level log line. Its only fields are client_id and family_id; neither the
token nor its digest appears. Repeated lines for the same client point either to
a stale refresh token held by that client or to a leaked token.

For a local database, use stl db status, stl db check, and stl db backup <path>.
Backups never overwrite an existing destination.
