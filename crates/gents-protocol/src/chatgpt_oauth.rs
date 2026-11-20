/// Codex-compatible OAuth client id.
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// Default OAuth issuer for ChatGPT/Codex accounts.
pub const DEFAULT_ISSUER: &str = "https://auth.openai.com";

pub fn token_endpoint(issuer: &str) -> String {
    format!("{}/oauth/token", issuer.trim_end_matches('/'))
}
