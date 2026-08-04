use crate::core_types::PlanType;
use crate::protocol::common::AuthMode;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Account {
    #[serde(rename = "apiKey", rename_all = "camelCase")]
    ApiKey {},

    #[serde(rename = "chatgpt", rename_all = "camelCase")]
    Chatgpt { email: String, plan_type: PlanType },

    #[serde(rename = "amazonBedrock", rename_all = "camelCase")]
    AmazonBedrock {},
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type")]
pub enum LoginAccountParams {
    #[serde(rename = "apiKey", rename_all = "camelCase")]
    ApiKey {
        #[serde(rename = "apiKey")]
        api_key: String,
    },
    #[serde(rename = "chatgpt", rename_all = "camelCase")]
    Chatgpt {
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        codex_streamlined_login: bool,
    },
    #[serde(rename = "chatgptDeviceCode")]
    ChatgptDeviceCode,
    /// [UNSTABLE] FOR OPENAI INTERNAL USE ONLY - DO NOT USE.
    /// The access token must contain the same scopes that Codex-managed ChatGPT auth tokens have.
    #[serde(rename = "chatgptAuthTokens", rename_all = "camelCase")]
    ChatgptAuthTokens {
        /// Access token (JWT) supplied by the client.
        /// This token is used for backend API requests and email extraction.
        access_token: String,
        /// Workspace/account identifier supplied by the client.
        chatgpt_account_id: String,
        /// Optional plan type supplied by the client.
        ///
        /// When `null`, Codex attempts to derive the plan type from access-token
        /// claims. If unavailable, the plan defaults to `unknown`.
        chatgpt_plan_type: Option<String>,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum LoginAccountResponse {
    #[serde(rename = "apiKey", rename_all = "camelCase")]
    ApiKey {},
    #[serde(rename = "chatgpt", rename_all = "camelCase")]
    Chatgpt {
        // Use plain String for identifiers to avoid TS/JSON Schema quirks around uuid-specific types.
        // Convert to/from UUIDs at the application layer as needed.
        login_id: String,
        /// URL the client should open in a browser to initiate the OAuth flow.
        auth_url: String,
    },
    #[serde(rename = "chatgptDeviceCode", rename_all = "camelCase")]
    ChatgptDeviceCode {
        // Use plain String for identifiers to avoid TS/JSON Schema quirks around uuid-specific types.
        // Convert to/from UUIDs at the application layer as needed.
        login_id: String,
        /// URL the client should open in a browser to complete device code authorization.
        verification_url: String,
        /// One-time code the user must enter after signing in.
        user_code: String,
    },
    #[serde(rename = "chatgptAuthTokens", rename_all = "camelCase")]
    ChatgptAuthTokens {},
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CancelLoginAccountParams {
    pub login_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CancelLoginAccountStatus {
    Canceled,
    NotFound,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CancelLoginAccountResponse {
    pub status: CancelLoginAccountStatus,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LogoutAccountResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ChatgptAuthTokensRefreshReason {
    /// Codex attempted a backend request and received `401 Unauthorized`.
    Unauthorized,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChatgptAuthTokensRefreshParams {
    pub reason: ChatgptAuthTokensRefreshReason,
    /// Workspace/account identifier that Codex was previously using.
    ///
    /// Clients that manage multiple accounts/workspaces can use this as a hint
    /// to refresh the token for the correct workspace.
    ///
    /// This may be `null` when the prior auth state did not include a workspace
    /// identifier (`chatgpt_account_id`).
    pub previous_account_id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChatgptAuthTokensRefreshResponse {
    pub access_token: String,
    pub chatgpt_account_id: String,
    pub chatgpt_plan_type: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GetAccountRateLimitsResponse {
    /// Backward-compatible single-bucket view; mirrors the historical payload.
    pub rate_limits: RateLimitSnapshot,
    /// Multi-bucket view keyed by metered `limit_id` (for example, `codex`).
    pub rate_limits_by_limit_id: Option<HashMap<String, RateLimitSnapshot>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SendAddCreditsNudgeEmailParams {
    pub credit_type: AddCreditsNudgeCreditType,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AddCreditsNudgeCreditType {
    Credits,
    UsageLimit,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SendAddCreditsNudgeEmailResponse {
    pub status: AddCreditsNudgeEmailStatus,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AddCreditsNudgeEmailStatus {
    Sent,
    CooldownActive,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GetAccountParams {
    /// When `true`, requests a proactive token refresh before returning.
    ///
    /// In managed auth mode this triggers the normal refresh-token flow. In
    /// external auth mode this flag is ignored. Clients should refresh tokens
    /// themselves and call `account/login/start` with `chatgptAuthTokens`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub refresh_token: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GetAccountResponse {
    pub account: Option<Account>,
    pub requires_openai_auth: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AccountUpdatedNotification {
    pub auth_mode: Option<AuthMode>,
    pub plan_type: Option<PlanType>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AccountRateLimitsUpdatedNotification {
    pub rate_limits: RateLimitSnapshot,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitSnapshot {
    pub limit_id: Option<String>,
    pub limit_name: Option<String>,
    pub primary: Option<RateLimitWindow>,
    pub secondary: Option<RateLimitWindow>,
    pub credits: Option<CreditsSnapshot>,
    pub plan_type: Option<PlanType>,
    pub rate_limit_reached_type: Option<RateLimitReachedType>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RateLimitReachedType {
    RateLimitReached,
    WorkspaceOwnerCreditsDepleted,
    WorkspaceMemberCreditsDepleted,
    WorkspaceOwnerUsageLimitReached,
    WorkspaceMemberUsageLimitReached,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitWindow {
    pub used_percent: i32,
    pub window_duration_mins: Option<i64>,
    pub resets_at: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CreditsSnapshot {
    pub has_credits: bool,
    pub unlimited: bool,
    pub balance: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AccountLoginCompletedNotification {
    // Use plain String for identifiers to avoid TS/JSON Schema quirks around uuid-specific types.
    // Convert to/from UUIDs at the application layer as needed.
    pub login_id: Option<String>,
    pub success: bool,
    pub error: Option<String>,
}
