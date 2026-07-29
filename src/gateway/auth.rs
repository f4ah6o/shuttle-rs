//! Gateway authentication boundary.
//!
//! Listener construction remains in the gateway configuration layer, while
//! request handlers depend on this narrow enum instead of environment details.

use std::sync::Arc;

use crate::oauth::{OAuthConfig, OAuthStore};

#[derive(Clone)]
pub(super) enum GatewayAuth {
    Bearer { token_env: String },
    OAuth(Arc<OAuthRuntime>),
    None,
}

#[derive(Clone)]
pub(super) struct OAuthRuntime {
    pub(super) config: OAuthConfig,
    pub(super) store: OAuthStore,
}
