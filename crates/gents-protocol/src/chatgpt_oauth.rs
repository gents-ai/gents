//! ChatGPT OAuth vocabulary shared by `gents-chatgpt-login` (the login/token
//! flows) and `gents::chatgpt_oauth_refresh` (the runtime refresh path).
//! Single owner (#1339) for the duplicated client id, refresh-endpoint
//! override variable, and token-endpoint construction rule.

/// Codex-compatible OAuth client id.
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// Default OAuth issuer for ChatGPT/Codex accounts.
pub const DEFAULT_ISSUER: &str = "https://auth.openai.com";

/// Environment variable that, when set to a non-empty value, overrides the
/// refresh-token endpoint (used in tests and self-hosted issuer setups).
pub const REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR: &str = "CODEX_REFRESH_TOKEN_URL_OVERRIDE";

/// Build the token endpoint for an OAuth issuer. Single owner of the
/// trailing-slash normalization and `/oauth/token` suffix shared by login
/// and runtime refresh flows.
pub fn token_endpoint(issuer: &str) -> String {
    format!("{}/oauth/token", issuer.trim_end_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_endpoint_normalizes_the_issuer_once() {
        assert_eq!(
            token_endpoint(DEFAULT_ISSUER),
            "https://auth.openai.com/oauth/token"
        );
        assert_eq!(
            token_endpoint("https://issuer.example/"),
            "https://issuer.example/oauth/token"
        );
    }
}
