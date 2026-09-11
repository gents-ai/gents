use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use crate::graphql::escape_graphql_string;

/// Complete selectable inference preset. Multiple profiles may select different
/// models or efforts through the same backend without duplicating credentials.
/// Sampling and execution settings live in their owning subobjects, referenced
/// by `sampling_id`/`execution_id`; unset references use canonical defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct InferenceProfile {
    pub agent_did: String,
    pub profile_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub description: Option<String>,
    /// Shared connectivity, authentication, discovery, and admission capacity.
    pub backend_id: String,
    /// Explicit selection from the backend's advertised options, not a model
    /// definition. Discovery refreshes must never rewrite this choice.
    pub model_name: String,
    /// Explicit backend-advertised effort selection. Unset uses the provider
    /// default; an unsupported explicit effort is an error.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub reasoning_effort: Option<crate::config::ReasoningEffort>,
    /// Model-specific context budget override. Resolution must use the selected
    /// model's capabilities, never another model's limits from the same backend.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub context_window: Option<i64>,
    /// Per-response output ceiling for requests through this profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub max_output_tokens: Option<i64>,
    /// Unset leaves optional sampling parameters to the provider defaults.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub sampling_id: Option<String>,
    /// Unset uses the existing owned-loop execution defaults.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub execution_id: Option<String>,
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub tags: Vec<String>,
}

impl InferenceProfile {
    /// Validate this profile's bounds — the single owner every write path
    /// (CLI desired state, self-config, `profile set`,
    /// `upsert_inference_profile`) calls. The profile subobject owns only its
    /// own rules: sampling bounds moved to `InferenceSampling::validate`,
    /// turn/stream/deadline/retry bounds to `InferenceExecution::validate`
    /// and `InferenceRetryPolicy` (#1430 split the flat profile). Reports
    /// every violated rule at once so a `config apply` user who broke two
    /// fields sees both in one round trip.
    pub fn validation_violations(&self) -> Vec<String> {
        let profile_id = self.profile_id.trim();
        let mut violations: Vec<String> = Vec::new();

        for (name, value) in [
            ("context_window", self.context_window),
            ("max_output_tokens", self.max_output_tokens),
        ] {
            if value.is_some_and(|value| value <= 0) {
                violations.push(format!(
                    "InferenceProfile {profile_id} {name} must be positive"
                ));
            }
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

pub fn default_inference_profile_id_for_behavior(behavior_id: &str) -> String {
    format!("{behavior_id}-profile")
}

pub async fn load_inference_profile(
    node: &EmbeddedNode,
    agent_did: &str,
    profile_id: &str,
) -> Result<Option<InferenceProfile>> {
    Ok(load_inference_profile_record(node, agent_did, profile_id)
        .await?
        .map(|(_, profile)| profile))
}

pub(crate) async fn load_inference_profile_record(
    node: &EmbeddedNode,
    agent_did: &str,
    profile_id: &str,
) -> Result<Option<(String, InferenceProfile)>> {
    crate::config_client::ConfigAccess::transact_local(
        node,
        None,
        "document_config.inference_profile.read",
        |txn| {
            Box::pin(async move {
                crate::config_client::read_desired_state_record_in_txn(
                    txn,
                    crate::collection::Collection::InferenceProfile,
                    agent_did,
                    profile_id,
                )
                .await?
                .map(|(doc_id, value)| Ok((doc_id, serde_json::from_value(value)?)))
                .transpose()
            })
        },
    )
    .await
}

pub async fn list_inference_profile_records(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<Vec<(String, InferenceProfile)>> {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let (fields, _) = crate::config_client::config_projection(
        crate::collection::Collection::InferenceProfile,
        None,
    )?;
    let fields = fields.join(" ");
    let query = format!(
        r#"{{
            InferenceProfile(
                filter: {{ agent_did: {{ _eq: "{escaped_agent_did}" }} }},
                order: {{ profile_id: ASC }}
            ) {{
                _docID
                {fields}
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("list InferenceProfile failed: {:?}", resp.errors);
    }

    let rows = resp
        .data
        .as_ref()
        .and_then(|data| data.get("InferenceProfile"))
        .and_then(serde_json::Value::as_array)
        .context("missing canonical configuration rows")?
        .clone();
    let mut seen = std::collections::HashSet::new();
    rows.into_iter()
        .map(|mut row| {
            let doc_id = row
                .as_object_mut()
                .context("configuration row must be an object")?
                .remove("_docID")
                .and_then(|value| value.as_str().map(str::to_owned))
                .context("configuration row missing physical ID")?;
            let document: InferenceProfile = serde_json::from_value(row)?;
            anyhow::ensure!(
                document.agent_did == agent_did,
                "foreign InferenceProfile row"
            );
            anyhow::ensure!(
                seen.insert(document.profile_id.clone()),
                "duplicate scoped InferenceProfile ID"
            );
            Ok((doc_id, document))
        })
        .collect()
}

/// Complete replacement through the existing transactional configuration owner.
pub async fn upsert_inference_profile(
    node: &EmbeddedNode,
    profile: &InferenceProfile,
) -> Result<()> {
    profile.validate()?;
    let value = serde_json::to_value(profile)?;
    let plan = crate::config_client::DesiredStateApplyPlan::new(vec![
        crate::config_client::DesiredStateApplyDocument {
            collection: crate::collection::Collection::InferenceProfile,
            add: value.clone(),
            update: value,
        },
    ])?;
    crate::config_client::ConfigAccess::transact_local(
        node,
        None,
        "document_config.inference_profile.upsert",
        |txn| {
            let plan = &plan;
            Box::pin(async move {
                crate::config_client::apply_desired_state_plan(txn, plan)
                    .await
                    .map(|_| ())
            })
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ReasoningEffort;
    use serde_json::json;

    #[test]
    fn effort_is_typed_and_explicit_limits_do_not_default_invalid_values() {
        let value = json!({"agent_did":"owner","profile_id":"profile","backend_id":"backend","model_name":"model","reasoning_effort":"high"});
        let mut profile: InferenceProfile = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(profile.reasoning_effort, Some(ReasoningEffort::High));
        assert_eq!(serde_json::to_value(&profile).unwrap(), value);
        for effort in ["", " ", "extreme"] {
            let mut invalid = value.clone();
            invalid["reasoning_effort"] = effort.into();
            assert!(serde_json::from_value::<InferenceProfile>(invalid).is_err());
        }
        for limit in [0, -1] {
            profile.context_window = Some(limit);
            profile.max_output_tokens = Some(limit);
            assert_eq!(profile.validation_violations().len(), 2);
        }
        profile.context_window = Some(1);
        profile.max_output_tokens = Some(1);
        assert!(profile.validate().is_ok());
    }
}
