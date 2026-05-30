use ng_core::permission::data_structure::Token;
use ng_core::permission::token_auth::TokenOrAuth;
use ng_infra::server::AuthChecker;

use crate::get::get_token;

/// Concrete `AuthChecker` that delegates to `ng_token::get::get_token`.
///
/// Registered via `ng_token::register_auth_checker()` during server startup.
pub struct TokenAuthChecker;

impl AuthChecker for TokenAuthChecker {
    fn check(&self, raw_token: &str) -> anyhow::Result<Token> {
        let token_or_auth = TokenOrAuth::from_full_token(raw_token)
            .map_err(|e| ng_core::error::NodegetError::ParseError(e.to_string()))?;

        // get_token is async but AuthChecker::check is sync.
        // We use tokio::runtime::Handle::block_on_point to bridge.
        // This is safe because we're called from within the tokio runtime
        // (during RPC handler execution).
        let handle = tokio::runtime::Handle::current();
        handle.block_on(get_token(&token_or_auth))
    }
}
