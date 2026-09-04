//! ChatGPT OAuth constants shared by `gents-chatgpt-login` (the login/token
//! flows) and `gents::chatgpt_oauth_refresh` (the runtime refresh path).
//! Single owner (#1339) — both previously held their own copies of the
//! client id and refresh endpoint.

/// Codex-compatible OAuth client id.
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// Default OAuth issuer for ChatGPT/Codex accounts.
pub const DEFAULT_ISSUER: &str = "https://auth.openai.com";

/// Environment variable that, when set to a non-empty value, overrides the
/// refresh-token endpoint (used in tests and self-hosted issuer setups).
pub const REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR: &str = "CODEX_REFRESH_TOKEN_URL_OVERRIDE";

/// The refresh-token endpoint under [`DEFAULT_ISSUER`]. Kept as its own
/// constant (rather than formatted at each call site) for callers that only
/// ever refresh against the default issuer; `gents-chatgpt-login`'s own
/// `refresh_tokens` still formats `{issuer}/oauth/token` directly since it
/// supports a caller-supplied issuer. `refresh_token_url_matches_default_issuer`
/// guards the two against drift.
pub const REFRESH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_token_url_matches_default_issuer() {
        assert_eq!(REFRESH_TOKEN_URL, format!("{DEFAULT_ISSUER}/oauth/token"));
    }
}
