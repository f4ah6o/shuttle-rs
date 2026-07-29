//! Gateway dependency direction and trust-boundary notes.
//!
//! Request flow:
//! listener/auth -> router -> protocol adapter (HTTP or MCP) -> GatewayService
//! -> Runner -> local SQLite or configured HTTP project.
//!
//! The service owns project selection and write/read policy. Runners are the
//! only backend-specific boundary. OAuth state is owned by the auth adapter;
//! it is never passed into service or runner calls.

pub(super) const REQUEST_FLOW: &str =
    "listener/auth -> router -> protocol adapter -> GatewayService -> Runner";
