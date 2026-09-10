use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use super::graphql_fields;
use super::serde_helpers;
use crate::config::{
    DEFAULT_CONTEXT_WINDOW, DEFAULT_DEADLINE_DURATION_SECS, DEFAULT_MAX_OUTPUT_TOKENS,
    DEFAULT_MAX_TURNS, DEFAULT_STREAM_BATCH_MS, DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS,
};
use crate::config_client::mint_recreate_identity_timestamp;
use crate::graphql::escape_graphql_string;

/// Complete selectable inference preset. Multiple profiles may select different
/// models or efforts through the same backend without duplicating credentials.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct InferenceProfile {
    pub agent_did: String,
    pub profile_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Shared connectivity, authentication, discovery, and admission capacity.
    pub backend_id: String,
    /// Explicit selection from the backend's advertised options, not a model
    /// definition. Discovery refreshes must never rewrite this choice.
    pub model_name: String,
    /// Unset uses the provider default; an explicit unsupported effort is an error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<crate::config::ReasoningEffort>,
    /// Model-specific context budget override. Resolution must use the selected
    /// model's capabilities, never another model's limits from the same backend.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<i64>,
    /// Unset leaves optional sampling parameters to the provider defaults.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling_id: Option<String>,
    /// Unset uses the existing owned-loop execution defaults.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tags: Vec<String>,
}

impl InferenceProfile {
    /// Validate this profile's bounds — the single owner every write path
    /// (CLI desired state, self-config, `profile set`,
    /// `upsert_inference_profile`) calls. Mirrors the historical `gents-cli`
    /// desired-state rules exactly so no case is lost by the move (#1331),
    /// and reports every violated rule at once so a `config apply` user who
    /// broke two fields sees both in one round trip.
    pub fn validation_violations(&self) -> Vec<String> {
        let profile_id = self.profile_id.trim();
        let mut violations: Vec<String> = Vec::new();

        let stream_liveness_timeout_secs = match self.stream_liveness_timeout_secs {
            Some(value) if value <= 0 => {
                violations.push(format!(
                    "InferenceProfile {profile_id} stream_liveness_timeout_secs must be positive"
                ));
                None
            }
            Some(value) => Some(value),
            None => Some(DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS as i64),
        };
        let deadline_duration_secs = match self.deadline_duration_secs {
            Some(value) if value <= 0 => {
                violations.push(format!(
                    "InferenceProfile {profile_id} deadline_duration_secs must be positive"
                ));
                None
            }
            Some(value) => Some(value),
            None => Some(DEFAULT_DEADLINE_DURATION_SECS as i64),
        };
        // Only compare the two bounds when both are individually valid — an
        // already-invalid bound would make this comparison noise on top of
        // the violation already reported above.
        if let (Some(stream_liveness_timeout_secs), Some(deadline_duration_secs)) =
            (stream_liveness_timeout_secs, deadline_duration_secs)
        {
            if stream_liveness_timeout_secs >= deadline_duration_secs {
                violations.push(format!(
                    "InferenceProfile {profile_id} stream_liveness_timeout_secs ({stream_liveness_timeout_secs}) must be less than deadline_duration_secs ({deadline_duration_secs})"
                ));
            }
        }
        if self.seed.is_some_and(|value| value < 0) {
            violations.push(format!(
                "InferenceProfile {profile_id} seed must be non-negative"
            ));
        }
        // Sampling bounds (#1331 fix round 1 — previously only enforced by
        // the `gents config profile set` imperative writer, not the owner).
        if self
            .top_p
            .is_some_and(|value| !(0.0..=1.0).contains(&value))
        {
            violations.push(format!(
                "InferenceProfile {profile_id} top_p must be within [0, 1]"
            ));
        }
        if self
            .min_p
            .is_some_and(|value| !(0.0..=1.0).contains(&value))
        {
            violations.push(format!(
                "InferenceProfile {profile_id} min_p must be within [0, 1]"
            ));
        }
        if self.top_k.is_some_and(|value| value <= 0) {
            violations.push(format!(
                "InferenceProfile {profile_id} top_k must be positive"
            ));
        }
        if self.repetition_penalty.is_some_and(|value| value <= 0.0) {
            violations.push(format!(
                "InferenceProfile {profile_id} repetition_penalty must be positive"
            ));
        }
        for (name, value) in [
            ("frequency_penalty", self.frequency_penalty),
            ("presence_penalty", self.presence_penalty),
        ] {
            if value.is_some_and(|value| !(-2.0..=2.0).contains(&value)) {
                violations.push(format!(
                    "InferenceProfile {profile_id} {name} must be within [-2, 2]"
                ));
            }
        }
        // Empty reasoning effort is unset: DefraDB may materialize nullable
        // strings as empty values, and exported manifests must round-trip.
        if self
            .reasoning_effort
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some_and(|value| {
                !matches!(
                    value,
                    "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra"
                )
            })
        {
            violations.push(format!(
                "InferenceProfile {profile_id} reasoning_effort must be one of: none, minimal, low, medium, high, xhigh, max, ultra"
            ));
        }

        violations
    }

    pub fn validate(&self) -> Result<()> {
        let violations = self.validation_violations();
        if violations.is_empty() {
            Ok(())
        } else {
            anyhow::bail!(violations.join("; "))
        }
    }
}

const DEFAULT_INFERENCE_PROFILE_LABEL: &str = "Default";

pub fn default_inference_profile_id_for_behavior(behavior_id: &str) -> String {
    format!("{behavior_id}-profile")
}

pub(super) fn default_inference_profile_for_behavior(behavior_id: &str) -> InferenceProfile {
    InferenceProfile {
        profile_id: default_inference_profile_id_for_behavior(behavior_id),
        display_name: Some(DEFAULT_INFERENCE_PROFILE_LABEL.to_string()),
        context_window: Some(DEFAULT_CONTEXT_WINDOW as i64),
        max_output_tokens: Some(DEFAULT_MAX_OUTPUT_TOKENS as i64),
        max_turns: Some(DEFAULT_MAX_TURNS as i64),
        temperature: Some(0.0),
        // Unset: inherit whatever the served model's generation_config.json
        // specifies. The default profile must not silently impose sampling the
        // operator never asked for.
        top_p: None,
        top_k: None,
        seed: None,
        min_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        repetition_penalty: None,
        reasoning_effort: None,
        stream_batch_ms: Some(DEFAULT_STREAM_BATCH_MS as i64),
        stream_liveness_timeout_secs: Some(DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS as i64),
        deadline_duration_secs: Some(DEFAULT_DEADLINE_DURATION_SECS as i64),
        retry_max_transport: None,
        retry_backoff_ms: None,
        retry_max_resample: None,
        retry_allow_repair: None,
        retry_interactive_max: None,
    }
}

pub(super) async fn create_default_inference_profile(
    node: &EmbeddedNode,
    behavior_id: &str,
) -> Result<InferenceProfile> {
    let profile = default_inference_profile_for_behavior(behavior_id);
    upsert_inference_profile(node, &profile).await?;
    Ok(profile)
}

pub async fn load_inference_profile(
    node: &EmbeddedNode,
    profile_id: &str,
) -> Result<Option<InferenceProfile>> {
    Ok(load_inference_profile_record(node, profile_id)
        .await?
        .map(|(_, profile)| profile))
}

pub(crate) async fn load_inference_profile_record(
    node: &EmbeddedNode,
    profile_id: &str,
) -> Result<Option<(String, InferenceProfile)>> {
    let escaped_profile_id = escape_graphql_string(profile_id);
    let query = format!(
        r#"{{
            InferenceProfile(
                filter: {{ profile_id: {{ _eq: "{escaped_profile_id}" }} }},
                limit: 1
            ) {{
                _docID
                profile_id
                display_name
                context_window
                max_output_tokens
                max_turns
                temperature
                top_p
                top_k
                seed
                min_p
                frequency_penalty
                presence_penalty
                repetition_penalty
                reasoning_effort
                stream_batch_ms
                stream_liveness_timeout_secs
                deadline_duration_secs
                retry_max_transport
                retry_backoff_ms
                retry_max_resample
                retry_allow_repair
                retry_interactive_max
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("query InferenceProfile failed: {:?}", resp.errors);
    }

    Ok(serde_helpers::first_row_with_doc_id(
        resp.data.as_ref(),
        "InferenceProfile",
    ))
}

pub(crate) async fn load_inference_profile_by_doc_id(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<Option<(String, InferenceProfile)>> {
    let escaped_doc_id = escape_graphql_string(doc_id);
    let query = format!(
        r#"{{
            InferenceProfile(
                filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                limit: 1
            ) {{
                _docID
                profile_id
                display_name
                context_window
                max_output_tokens
                max_turns
                temperature
                top_p
                top_k
                seed
                min_p
                frequency_penalty
                presence_penalty
                repetition_penalty
                reasoning_effort
                stream_batch_ms
                stream_liveness_timeout_secs
                deadline_duration_secs
                retry_max_transport
                retry_backoff_ms
                retry_max_resample
                retry_allow_repair
                retry_interactive_max
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("query InferenceProfile by _docID failed: {:?}", resp.errors);
    }

    Ok(serde_helpers::first_row_with_doc_id(
        resp.data.as_ref(),
        "InferenceProfile",
    ))
}

pub async fn list_inference_profile_records(
    node: &EmbeddedNode,
) -> Result<Vec<(String, InferenceProfile)>> {
    let query = r#"{
            InferenceProfile(order: { profile_id: ASC }) {
                _docID
                profile_id
                display_name
                context_window
                max_output_tokens
                max_turns
                temperature
                top_p
                top_k
                seed
                min_p
                frequency_penalty
                presence_penalty
                repetition_penalty
                reasoning_effort
                stream_batch_ms
                stream_liveness_timeout_secs
                deadline_duration_secs
                retry_max_transport
                retry_backoff_ms
                retry_max_resample
                retry_allow_repair
                retry_interactive_max
            }
        }"#;

    let resp = node.execute(query).await;
    if resp.has_errors() {
        anyhow::bail!("list InferenceProfile failed: {:?}", resp.errors);
    }

    Ok(serde_helpers::rows_with_doc_id(
        resp.data.as_ref(),
        "InferenceProfile",
    ))
}

pub async fn upsert_inference_profile(
    node: &EmbeddedNode,
    profile: &InferenceProfile,
) -> Result<()> {
    crate::config_client::ConfigAccess::transact_local(
        node,
        None,
        "document_config.inference_profile.upsert",
        |txn| {
            Box::pin(async move {
                crate::config_client::effective_inference_profile(txn, profile)
                    .await?
                    .validate()?;
                txn.execute(&upsert_inference_profile_mutation(profile))
                    .await?;
                Ok(())
            })
        },
    )
    .await
}

pub(crate) fn upsert_inference_profile_mutation(profile: &InferenceProfile) -> String {
    let escaped_profile_id = escape_graphql_string(&profile.profile_id);

    let add_fields = vec![
        Some(format!(r#"profile_id: "{escaped_profile_id}""#)),
        graphql_fields::graphql_string_field("display_name", profile.display_name.as_deref()),
        graphql_fields::graphql_optional_int_field("context_window", profile.context_window),
        graphql_fields::graphql_optional_int_field("max_output_tokens", profile.max_output_tokens),
        graphql_fields::graphql_optional_int_field("max_turns", profile.max_turns),
        graphql_fields::graphql_optional_float_field("temperature", profile.temperature),
        graphql_fields::graphql_optional_float_field("top_p", profile.top_p),
        graphql_fields::graphql_optional_int_field("top_k", profile.top_k),
        graphql_fields::graphql_optional_int_field("seed", profile.seed),
        graphql_fields::graphql_optional_float_field("min_p", profile.min_p),
        graphql_fields::graphql_optional_float_field(
            "frequency_penalty",
            profile.frequency_penalty,
        ),
        graphql_fields::graphql_optional_float_field("presence_penalty", profile.presence_penalty),
        graphql_fields::graphql_optional_float_field(
            "repetition_penalty",
            profile.repetition_penalty,
        ),
        graphql_fields::graphql_string_field(
            "reasoning_effort",
            profile.reasoning_effort.as_deref(),
        ),
        graphql_fields::graphql_optional_int_field("stream_batch_ms", profile.stream_batch_ms),
        graphql_fields::graphql_optional_int_field(
            "stream_liveness_timeout_secs",
            profile.stream_liveness_timeout_secs,
        ),
        graphql_fields::graphql_optional_int_field(
            "deadline_duration_secs",
            profile.deadline_duration_secs,
        ),
        graphql_fields::graphql_optional_int_field(
            "retry_max_transport",
            profile.retry_max_transport,
        ),
        graphql_fields::graphql_int_list_field(
            "retry_backoff_ms",
            profile.retry_backoff_ms.as_deref(),
        ),
        graphql_fields::graphql_optional_int_field(
            "retry_max_resample",
            profile.retry_max_resample,
        ),
        graphql_fields::graphql_optional_bool_field(
            "retry_allow_repair",
            profile.retry_allow_repair,
        ),
        graphql_fields::graphql_optional_int_field(
            "retry_interactive_max",
            profile.retry_interactive_max,
        ),
        Some(format!(
            r#"updated_at: "{}""#,
            escape_graphql_string(&mint_recreate_identity_timestamp())
        )),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(",\n                    ");

    let update_fields = vec![
        graphql_fields::graphql_string_field("display_name", profile.display_name.as_deref()),
        graphql_fields::graphql_optional_int_field("context_window", profile.context_window),
        graphql_fields::graphql_optional_int_field("max_output_tokens", profile.max_output_tokens),
        graphql_fields::graphql_optional_int_field("max_turns", profile.max_turns),
        graphql_fields::graphql_optional_float_field("temperature", profile.temperature),
        graphql_fields::graphql_optional_float_field("top_p", profile.top_p),
        graphql_fields::graphql_optional_int_field("top_k", profile.top_k),
        graphql_fields::graphql_optional_int_field("seed", profile.seed),
        graphql_fields::graphql_optional_float_field("min_p", profile.min_p),
        graphql_fields::graphql_optional_float_field(
            "frequency_penalty",
            profile.frequency_penalty,
        ),
        graphql_fields::graphql_optional_float_field("presence_penalty", profile.presence_penalty),
        graphql_fields::graphql_optional_float_field(
            "repetition_penalty",
            profile.repetition_penalty,
        ),
        graphql_fields::graphql_string_field(
            "reasoning_effort",
            profile.reasoning_effort.as_deref(),
        ),
        graphql_fields::graphql_optional_int_field("stream_batch_ms", profile.stream_batch_ms),
        graphql_fields::graphql_optional_int_field(
            "stream_liveness_timeout_secs",
            profile.stream_liveness_timeout_secs,
        ),
        graphql_fields::graphql_optional_int_field(
            "deadline_duration_secs",
            profile.deadline_duration_secs,
        ),
        graphql_fields::graphql_optional_int_field(
            "retry_max_transport",
            profile.retry_max_transport,
        ),
        graphql_fields::graphql_int_list_field(
            "retry_backoff_ms",
            profile.retry_backoff_ms.as_deref(),
        ),
        graphql_fields::graphql_optional_int_field(
            "retry_max_resample",
            profile.retry_max_resample,
        ),
        graphql_fields::graphql_optional_bool_field(
            "retry_allow_repair",
            profile.retry_allow_repair,
        ),
        graphql_fields::graphql_optional_int_field(
            "retry_interactive_max",
            profile.retry_interactive_max,
        ),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(",\n                    ");

    format!(
        r#"mutation {{
            upsert_InferenceProfile(
                filter: {{ profile_id: {{ _eq: "{escaped_profile_id}" }} }},
                add: {{
                    {add_fields}
                }},
                update: {{
                    {update_fields}
                }}
            ) {{ _docID }}
        }}"#
    )
}
