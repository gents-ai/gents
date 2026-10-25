use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use super::error::DocumentConfigError;
use crate::graphql::{escape_graphql_string, graphql_with_transaction_retry};

/// Complete selectable inference preset; multiple profiles may select different
/// models or efforts through the same backend without duplicating credentials.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct InferenceProfile {
    pub node_did: String,
    pub profile_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub description: Option<String>,
    /// Shared connectivity, authentication, discovery, and admission capacity.
    pub backend_id: String,
    /// Explicit selection from the backend's advertised options; discovery
    /// refreshes must never rewrite it.
    pub model_name: String,
    /// Explicit backend-advertised effort selection; unset uses the provider default.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub reasoning_effort: Option<crate::config::ReasoningEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub service_tier: Option<crate::config::ServiceTier>,
    /// Model-specific context budget override; resolution uses the selected
    /// model's capabilities.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub context_window: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub max_output_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub sampling_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub execution_id: Option<String>,
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
    /// calls; reports every violated rule at once.
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

    pub fn validate(&self) -> std::result::Result<(), DocumentConfigError> {
        let violations = self.validation_violations();
        if violations.is_empty() {
            Ok(())
        } else {
            Err(DocumentConfigError::InferenceProfileViolations {
                violations: violations.join("; "),
            })
        }
    }
}

pub fn default_inference_profile_id_for_agent(agent_id: &str) -> String {
    format!("{agent_id}-profile")
}

pub fn default_inference_profile_id_for_node(node_did: &str) -> String {
    format!("{node_did}:default-profile")
}

pub async fn load_inference_profile(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    profile_id: &str,
) -> Result<Option<InferenceProfile>> {
    Ok(load_inference_profile_record(node, node_did, profile_id)
        .await?
        .map(|(_, profile)| profile))
}

pub(crate) async fn load_inference_profile_record(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    profile_id: &str,
) -> Result<Option<(String, InferenceProfile)>> {
    crate::config_client::ConfigAccess::transact_local_readonly(
        node,
        None,
        "document_config.inference_profile.read",
        |txn| {
            Box::pin(async move {
                crate::config_client::read_desired_state_record_in_txn(
                    txn,
                    crate::collection::Collection::InferenceProfile,
                    node_did,
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
    node_did: &str,
) -> Result<Vec<(String, InferenceProfile)>> {
    let escaped_node_did = escape_graphql_string(node_did);
    let (fields, _) = crate::config_client::config_projection(
        crate::collection::Collection::InferenceProfile,
        None,
    )?;
    let fields = fields.join(" ");
    let query = format!(
        r#"{{
            InferenceProfile(
                filter: {{ node_did: {{ _eq: "{escaped_node_did}" }} }},
                order: {{ profile_id: ASC }}
            ) {{
                _docID
                {fields}
            }}
        }}"#
    );

    let resp = graphql_with_transaction_retry(node, &query, "list InferenceProfile").await?;

    let rows = resp
        .data
        .as_ref()
        .and_then(|data| data.get("InferenceProfile"))
        .and_then(serde_json::Value::as_array)
        .ok_or(DocumentConfigError::InferenceProfileRowsMissing)?
        .clone();
    let mut seen = rapidhash::RapidHashSet::default();
    rows.into_iter()
        .map(|mut row| {
            let doc_id = row
                .as_object_mut()
                .ok_or(DocumentConfigError::ConfigurationRowNotObject)?
                .remove("_docID")
                .and_then(|value| value.as_str().map(str::to_owned))
                .ok_or(DocumentConfigError::ConfigurationRowMissingPhysicalId)?;
            let document: InferenceProfile = serde_json::from_value(row)?;
            if document.node_did != node_did {
                return Err(DocumentConfigError::ForeignInferenceProfileRow.into());
            }
            if !seen.insert(document.profile_id.clone()) {
                return Err(DocumentConfigError::DuplicateScopedInferenceProfile.into());
            }
            Ok((doc_id, document))
        })
        .collect()
}

pub async fn upsert_inference_profile(
    node: &std::sync::Arc<EmbeddedNode>,
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
    Ok(crate::config_client::ConfigAccess::transact_local_write(
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
    .await?)
}
