use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

const DEFAULT_LIST_LIMIT: u32 = 20;
const MAX_LIST_LIMIT: u32 = 50;

pub(crate) const CHARS_PER_TOKEN_ESTIMATE: u32 = 4;
const DEFAULT_READ_PROCESS_MAX_TOKENS: u32 = 4096;
/// Bound a single returned page's context and allocation; retained output is
/// fetched in later pages instead of widening this ceiling.
const MAX_READ_PROCESS_MAX_TOKENS: u32 = 65536;
/// Keep each page large enough to advance its byte offset.
const MIN_READ_PROCESS_MAX_TOKENS: u32 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ListStatusFilter {
    #[default]
    Running,
    Terminal,
    All,
}

fn default_list_limit() -> u32 {
    DEFAULT_LIST_LIMIT
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ListBackgroundToolsArgs {
    #[serde(default)]
    pub(crate) status: ListStatusFilter,
    #[serde(default = "default_list_limit")]
    pub(crate) limit: u32,
}

impl ListBackgroundToolsArgs {
    pub(crate) fn validated_limit(&self) -> u32 {
        self.limit.clamp(1, MAX_LIST_LIMIT)
    }
}

/// The caller selects a budget for each page through `max_tokens`; this is
/// independent of the process output retention capacity.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ReadToolOutputArgs {
    pub(crate) tool_call_id: String,
    #[serde(default)]
    pub(crate) offset: u64,
    #[serde(default = "default_read_process_max_tokens")]
    pub(crate) max_tokens: u32,
}

fn default_read_process_max_tokens() -> u32 {
    DEFAULT_READ_PROCESS_MAX_TOKENS
}

impl ReadToolOutputArgs {
    pub(crate) fn validated_max_tokens(&self) -> u32 {
        self.max_tokens
            .clamp(MIN_READ_PROCESS_MAX_TOKENS, MAX_READ_PROCESS_MAX_TOKENS)
    }

    pub(crate) fn validated_max_bytes(&self) -> usize {
        (self.validated_max_tokens() as usize).saturating_mul(CHARS_PER_TOKEN_ESTIMATE as usize)
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ListBackgroundToolsEntry {
    pub(crate) tool_call_id: String,
    pub(crate) tool_name: String,
    pub(crate) deployment_id: String,
    pub(crate) await_mode: String,
    /// Parsed lifecycle token, or "unknown" when absent/unparseable.
    pub(crate) status: String,
    /// Verbatim persisted lifecycle_state token (may be Some("garbage")).
    pub(crate) lifecycle_state_raw: Option<String>,
    pub(crate) started_at: Option<DateTime<Utc>>,
    pub(crate) completed_at: Option<DateTime<Utc>>,
    /// Active age; None when terminal/unknown or timestamps unusable.
    pub(crate) elapsed_seconds: Option<i64>,
    /// Stable terminal duration; None unless both endpoints parse and are
    /// ordered.
    pub(crate) terminal_elapsed_seconds: Option<i64>,
    /// Total volume of the combined canonical output. This is a truthful
    /// count of combined bytes only; it is NOT stdout accounting.
    pub(crate) available_bytes: u64,
    /// Trailing preview of the combined canonical output, char-boundary
    /// safe. Binds only what is embedded in this response: the full
    /// canonical output is fetched server-side before truncation, so a
    /// bounded preview does NOT imply a bounded fetch. Bounded retrieval
    /// drilldown remains read_process.
    pub(crate) preview: Option<String>,
    pub(crate) preview_truncated: bool,
    /// DEPRECATED COMBINED LEGACY — KNOWN SEMANTIC BUG, PRE-EXISTING AND
    /// UNFIXED HERE: this field has always carried the combined
    /// stdout+stderr canonical volume (never stdout-only), and
    /// `stderr_bytes` has always been hard-coded 0. Kept only for wire
    /// compatibility, value unchanged (== available_bytes). It is not
    /// truthful stdout accounting, and this commit does not fix the split.
    pub(crate) stdout_bytes: u64,
    /// DEPRECATED COMBINED LEGACY: always 0; stderr was never tracked. See
    /// `stdout_bytes` for the open pre-existing semantic bug.
    pub(crate) stderr_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ListBackgroundToolsResponse {
    pub(crate) read_at: DateTime<Utc>,
    pub(crate) truncated: bool,
    pub(crate) entries: Vec<ListBackgroundToolsEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ReadToolOutputResponse {
    pub(crate) tool_call_id: String,
    pub(crate) tool_name: String,
    pub(crate) status: String,
    pub(crate) output: String,
    pub(crate) next_offset: u64,
    pub(crate) first_available_offset: u64,
    pub(crate) total_bytes: u64,
    pub(crate) has_more: bool,
    pub(crate) exited: bool,
    pub(crate) exit_code: Option<i32>,
}
