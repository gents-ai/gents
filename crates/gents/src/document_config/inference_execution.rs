use serde::{Deserialize, Serialize};

/// Owned-loop execution settings. These are configuration limits; active request
/// deadlines, retry counters, and token ledgers remain owned execution state.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InferenceExecution {
    pub agent_did: String,
    pub execution_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<i64>,
    /// Optional aggregate provider-token limit for one physical request, shared by
    /// its inference and compaction calls. Positive when set; unset is unlimited.
    /// The execution owner pins the resolved limit and rehydrates usage on restart.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_total_tokens: Option<i64>,
    /// Persistence batching cadence, not provider latency. Existing default 1,000ms.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_batch_ms: Option<i64>,
    /// Maximum provider-stream silence. Existing default 1,800s.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_liveness_timeout_secs: Option<i64>,
    /// Overall claimed-request duration. Existing default 86,400s; includes tools.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline_duration_secs: Option<i64>,
    /// Unset uses the existing retry policy for the request's execution origin.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_policy_id: Option<String>,
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tags: Vec<String>,
}

/// Retry policy selected by InferenceExecution. Unset fields preserve the existing
/// owned-loop defaults and classification; retries share the request deadline/budget.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InferenceRetryPolicy {
    pub agent_did: String,
    pub retry_policy_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_transport_retries: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backoff_ms: Option<Vec<i64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_resample_retries: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_repair: Option<bool>,
    /// Default interactive successor ceiling, resolved onto the initial request.
    /// Its retry chain retains that ceiling; this is not a human approval gate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interactive_max_retries: Option<i64>,
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tags: Vec<String>,
}
