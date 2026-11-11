use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

const DEFAULT_LIST_LIMIT: u32 = 20;
const MAX_LIST_LIMIT: u32 = 50;

pub const CHARS_PER_TOKEN_ESTIMATE: u32 = 4;
const DEFAULT_READ_PROCESS_MAX_TOKENS: u32 = 4096;
/// Bound a single returned page's context and allocation; retained output is
/// fetched in later pages instead of widening this ceiling.
const MAX_READ_PROCESS_MAX_TOKENS: u32 = 65536;
/// Keep each page large enough to advance its byte offset.
const MIN_READ_PROCESS_MAX_TOKENS: u32 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ListStatusFilter {
    #[default]
    Running,
    Terminal,
    All,
}

fn default_list_limit() -> u32 {
    DEFAULT_LIST_LIMIT
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListBackgroundToolsArgs {
    #[serde(default)]
    pub status: ListStatusFilter,
    #[serde(default = "default_list_limit")]
    pub limit: u32,
}

impl ListBackgroundToolsArgs {
    pub fn validated_limit(&self) -> u32 {
        self.limit.clamp(1, MAX_LIST_LIMIT)
    }
}

/// The caller selects a budget for each page through `max_tokens`; this is
/// independent of the process output retention capacity.
#[derive(Debug, Clone, Deserialize)]
pub struct ReadToolOutputArgs {
    pub tool_call_id: String,
    #[serde(default)]
    pub offset: u64,
    #[serde(default = "default_read_process_max_tokens")]
    pub max_tokens: u32,
}

fn default_read_process_max_tokens() -> u32 {
    DEFAULT_READ_PROCESS_MAX_TOKENS
}

impl ReadToolOutputArgs {
    pub fn validated_max_tokens(&self) -> u32 {
        self.max_tokens
            .clamp(MIN_READ_PROCESS_MAX_TOKENS, MAX_READ_PROCESS_MAX_TOKENS)
    }

    pub fn validated_max_bytes(&self) -> usize {
        (self.validated_max_tokens() as usize).saturating_mul(CHARS_PER_TOKEN_ESTIMATE as usize)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ListBackgroundToolsEntry {
    pub tool_call_id: String,
    pub tool_name: String,
    pub deployment_id: String,
    pub await_mode: String,
    /// Parsed lifecycle token, or "unknown" when absent/unparseable.
    pub status: String,
    /// Verbatim persisted lifecycle_state token (may be Some("garbage")).
    pub lifecycle_state_raw: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    /// Active age; None when terminal/unknown or timestamps unusable.
    pub elapsed_seconds: Option<i64>,
    /// Stable terminal duration; None unless both endpoints parse and are
    /// ordered.
    pub terminal_elapsed_seconds: Option<i64>,
    /// Total volume of the combined canonical output. This is a truthful
    /// count of combined bytes only; it is NOT stdout accounting.
    pub available_bytes: u64,
    /// Trailing preview of the combined canonical output, char-boundary
    /// safe. Binds only what is embedded in this response: the full
    /// canonical output is fetched server-side before truncation, so a
    /// bounded preview does NOT imply a bounded fetch. Bounded retrieval
    /// drilldown remains read_process.
    pub preview: Option<String>,
    pub preview_truncated: bool,
    /// DEPRECATED COMBINED LEGACY — KNOWN SEMANTIC BUG, PRE-EXISTING AND
    /// UNFIXED HERE: this field has always carried the combined
    /// stdout+stderr canonical volume (never stdout-only), and
    /// `stderr_bytes` has always been hard-coded 0. Kept only for wire
    /// compatibility, value unchanged (== available_bytes). It is not
    /// truthful stdout accounting, and this commit does not fix the split.
    pub stdout_bytes: u64,
    /// DEPRECATED COMBINED LEGACY: always 0; stderr was never tracked. See
    /// `stdout_bytes` for the open pre-existing semantic bug.
    pub stderr_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListBackgroundToolsResponse {
    pub read_at: DateTime<Utc>,
    pub truncated: bool,
    pub entries: Vec<ListBackgroundToolsEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReadToolOutputResponse {
    pub tool_call_id: String,
    pub tool_name: String,
    pub status: String,
    pub output: String,
    pub next_offset: u64,
    pub first_available_offset: u64,
    pub total_bytes: u64,
    pub has_more: bool,
    pub exited: bool,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WaitToolArgs {
    pub tool_call_id: String,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CancelToolArgs {
    pub tool_call_id: String,
    #[serde(default)]
    pub reason: Option<String>,
}
