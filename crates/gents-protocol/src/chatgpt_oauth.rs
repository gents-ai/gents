/// Codex-compatible OAuth client id.
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// Default OAuth issuer for ChatGPT/Codex accounts.
pub const DEFAULT_ISSUER: &str = "https://auth.openai.com";

/// Overrides the refresh-token endpoint when set non-empty.
pub const REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR: &str = "CODEX_REFRESH_TOKEN_URL_OVERRIDE";

/// Trailing-slash normalization plus `/oauth/token`.
pub fn token_endpoint(issuer: &str) -> String {
    format!("{}/oauth/token", issuer.trim_end_matches('/'))
}

