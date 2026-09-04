use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use crate::graphql::escape_graphql_string;

use super::graphql_fields;
use super::references::ConfigReferences;
use super::serde_helpers::{first_row_with_doc_id, rows_with_doc_id};

const DEFAULT_BEHAVIOR_LABEL: &str = "Default";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentBehavior {
    pub behavior_id: String,
    pub agent_did: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub summary: Option<String>,
    pub system_prompt: Option<String>,
    pub request_context_template: Option<String>,
    pub backend_id: Option<String>,
    pub model_name: Option<String>,
    pub tool_selection_id: Option<String>,
    pub inference_profile_id: Option<String>,
    pub compaction_strategy: Option<String>,
    pub compaction_threshold: Option<f64>,
    pub enabled: bool,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_string_vec_or_null"
    )]
    pub skill_refs: Vec<String>,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_string_vec_or_null"
    )]
    pub skill_excludes: Vec<String>,
    pub created_at: Option<String>,
}

impl AgentBehavior {
    /// Every violated reference rule, in rule-check order — the single
    /// owner every write path (CLI desired state, self-config's
    /// `configure_behavior`) calls. Checks that `backend_id`, `model_name`
    /// (against the backend's advertised models), `tool_selection_id`,
    /// `inference_profile_id`, and every `skill_refs`/`skill_excludes` entry
    /// name something that actually exists in `refs`. Structural checks
    /// (empty ids, ownership, template syntax) stay with each write path —
    /// they are shape checks on the manifest/patch, not document
    /// references (#1331).
    ///
    /// Reports every violation at once, not just the first — a behavior can
    /// dangle on more than one reference simultaneously (e.g. a missing
    /// backend, tool selection, and profile all at once), and desired
    /// state's per-behavior error list should surface each as its own entry
    /// rather than hide the rest behind whichever was checked first. Empty
    /// means every reference is valid.
    pub fn reference_violations(&self, refs: &ConfigReferences) -> Vec<String> {
        fn non_empty(value: Option<&str>) -> Option<&str> {
            value.map(str::trim).filter(|value| !value.is_empty())
        }

        let mut violations = Vec::new();

        if let Some(backend_id) = non_empty(self.backend_id.as_deref()) {
            match refs.backends.get(backend_id) {
                None => violations.push(format!(
                    "behavior {} references missing backend_id {}",
                    self.behavior_id, backend_id
                )),
                Some(advertised) => {
                    if let Some(model_name) = non_empty(self.model_name.as_deref()) {
                        if !advertised.is_empty()
                            && !advertised.iter().any(|model| model == model_name)
                        {
                            violations.push(format!(
                                "behavior {} selects model {} which backend {} does not advertise",
                                self.behavior_id, model_name, backend_id
                            ));
                        }
                    }
                }
            }
        }

        if let Some(selection_id) = non_empty(self.tool_selection_id.as_deref()) {
            if !refs.tool_selections.contains(selection_id) {
                violations.push(format!(
                    "behavior {} references missing tool_selection_id {}",
                    self.behavior_id, selection_id
                ));
            }
        }

        if let Some(profile_id) = non_empty(self.inference_profile_id.as_deref()) {
            if !refs.profiles.contains(profile_id) {
                violations.push(format!(
                    "behavior {} references missing inference_profile_id {}",
                    self.behavior_id, profile_id
                ));
            }
        }

        for skill_ref in &self.skill_refs {
            let skill_ref = skill_ref.trim();
            if !skill_ref.is_empty() && !refs.skills.contains(skill_ref) {
                violations.push(format!(
                    "behavior {} references missing skill_ref {} (import the skill first)",
                    self.behavior_id, skill_ref
                ));
            }
        }
        for skill_exclude in &self.skill_excludes {
            let skill_exclude = skill_exclude.trim();
            if !skill_exclude.is_empty() && !refs.skills.contains(skill_exclude) {
                violations.push(format!(
                    "behavior {} references missing skill_exclude {}",
                    self.behavior_id, skill_exclude
                ));
            }
        }

        violations
    }

    /// `Result` wrapper over [`Self::reference_violations`] for callers that
    /// just want pass/fail with one informative message (self-config,
    /// the CLI's raw-write doors) — every violation joined into one error.
    /// Desired state calls [`Self::reference_violations`] directly instead,
    /// so each violation becomes its own entry in its per-manifest error
    /// list.
    pub fn validate_references(&self, refs: &ConfigReferences) -> Result<()> {
        let violations = self.reference_violations(refs);
        if violations.is_empty() {
            Ok(())
        } else {
            anyhow::bail!(violations.join("; "))
        }
    }
}

pub async fn load_agent_behavior(
    node: &EmbeddedNode,
    behavior_id: &str,
) -> Result<Option<AgentBehavior>> {
    Ok(load_agent_behavior_record(node, behavior_id)
        .await?
        .map(|(_, behavior)| behavior))
}

pub(crate) async fn load_agent_behavior_record(
    node: &EmbeddedNode,
    behavior_id: &str,
) -> Result<Option<(String, AgentBehavior)>> {
    let escaped_behavior_id = escape_graphql_string(behavior_id);
    let query = format!(
        r#"{{
            AgentBehavior(
                filter: {{ behavior_id: {{ _eq: "{escaped_behavior_id}" }} }},
                limit: 1
            ) {{
                _docID
                behavior_id
                agent_did
                display_name
                description
                summary
                system_prompt
                request_context_template
                backend_id
                model_name
                tool_selection_id
                inference_profile_id
                compaction_strategy
                compaction_threshold
                enabled
                skill_refs
                skill_excludes
                created_at
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("query AgentBehavior failed: {:?}", resp.errors);
    }

    Ok(first_row_with_doc_id(resp.data.as_ref(), "AgentBehavior"))
}

pub(crate) async fn load_agent_behavior_by_doc_id(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<Option<(String, AgentBehavior)>> {
    let escaped_doc_id = escape_graphql_string(doc_id);
    let query = format!(
        r#"{{
            AgentBehavior(
                filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                limit: 1
            ) {{
                _docID
                behavior_id
                agent_did
                display_name
                description
                summary
                system_prompt
                request_context_template
                backend_id
                model_name
                tool_selection_id
                inference_profile_id
                compaction_strategy
                compaction_threshold
                enabled
                skill_refs
                skill_excludes
                created_at
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("query AgentBehavior by _docID failed: {:?}", resp.errors);
    }

    Ok(first_row_with_doc_id(resp.data.as_ref(), "AgentBehavior"))
}

pub async fn list_agent_behaviors(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<Vec<AgentBehavior>> {
    Ok(list_agent_behavior_records(node, agent_did)
        .await?
        .into_iter()
        .map(|(_, behavior)| behavior)
        .collect())
}

pub(crate) async fn list_agent_behavior_records(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<Vec<(String, AgentBehavior)>> {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            AgentBehavior(
                filter: {{ agent_did: {{ _eq: "{escaped_agent_did}" }} }},
                order: {{ created_at: ASC }}
            ) {{
                _docID
                behavior_id
                agent_did
                display_name
                description
                summary
                system_prompt
                request_context_template
                backend_id
                model_name
                tool_selection_id
                inference_profile_id
                compaction_strategy
                compaction_threshold
                enabled
                skill_refs
                skill_excludes
                created_at
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("list AgentBehavior failed: {:?}", resp.errors);
    }

    Ok(rows_with_doc_id(resp.data.as_ref(), "AgentBehavior"))
}

pub async fn upsert_agent_behavior(node: &EmbeddedNode, behavior: &AgentBehavior) -> Result<()> {
    let txn = crate::config_client::ConfigApplyTxn::begin_local(node, None).await?;
    let result = async {
        let mut effective = behavior.clone();
        if let Some(existing) =
            crate::config_client::load_agent_behavior_in_txn(&txn, &behavior.behavior_id).await?
        {
            // This legacy full-row encoder never writes the skill arrays, so
            // those two fields remain stored state during an update.
            effective.skill_refs = existing.skill_refs;
            effective.skill_excludes = existing.skill_excludes;
        } else {
            // This legacy encoder never writes the skill arrays on create,
            // so validate the empty defaults DefraDB will persist.
            effective.skill_refs.clear();
            effective.skill_excludes.clear();
        }
        let refs = ConfigReferences::load_in_txn(&txn, &effective.agent_did).await?;
        effective.validate_references(&refs)?;
        txn.execute(&upsert_agent_behavior_mutation(behavior))
            .await?;
        Ok(())
    }
    .await;
    match result {
        Ok(()) => txn.commit().await,
        Err(error) => {
            let _ = txn.discard().await;
            Err(error)
        }
    }
}

fn upsert_agent_behavior_mutation(behavior: &AgentBehavior) -> String {
    let escaped_behavior_id = escape_graphql_string(&behavior.behavior_id);
    let escaped_agent_did = escape_graphql_string(&behavior.agent_did);
    let created_at = behavior
        .created_at
        .as_deref()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

    let add_fields = vec![
        Some(format!(r#"behavior_id: "{escaped_behavior_id}""#)),
        Some(format!(r#"agent_did: "{escaped_agent_did}""#)),
        graphql_fields::graphql_string_field("display_name", behavior.display_name.as_deref()),
        graphql_fields::graphql_string_field("description", behavior.description.as_deref()),
        graphql_fields::graphql_string_field("summary", behavior.summary.as_deref()),
        graphql_fields::graphql_string_field("system_prompt", behavior.system_prompt.as_deref()),
        graphql_fields::graphql_string_field(
            "request_context_template",
            behavior.request_context_template.as_deref(),
        ),
        graphql_fields::graphql_string_field("backend_id", behavior.backend_id.as_deref()),
        graphql_fields::graphql_string_field("model_name", behavior.model_name.as_deref()),
        graphql_fields::graphql_string_field(
            "tool_selection_id",
            behavior.tool_selection_id.as_deref(),
        ),
        graphql_fields::graphql_string_field(
            "inference_profile_id",
            behavior.inference_profile_id.as_deref(),
        ),
        graphql_fields::graphql_string_field(
            "compaction_strategy",
            behavior.compaction_strategy.as_deref(),
        ),
        graphql_fields::graphql_optional_float_field(
            "compaction_threshold",
            behavior.compaction_threshold,
        ),
        Some(format!(
            "enabled: {}",
            graphql_fields::graphql_bool(behavior.enabled)
        )),
        Some(format!(
            r#"created_at: "{}""#,
            escape_graphql_string(created_at.as_str())
        )),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(",\n                    ");

    let update_fields = vec![
        Some(format!(r#"agent_did: "{escaped_agent_did}""#)),
        graphql_fields::graphql_string_field("display_name", behavior.display_name.as_deref()),
        graphql_fields::graphql_string_field("description", behavior.description.as_deref()),
        graphql_fields::graphql_string_field("summary", behavior.summary.as_deref()),
        graphql_fields::graphql_string_field("system_prompt", behavior.system_prompt.as_deref()),
        graphql_fields::graphql_string_field(
            "request_context_template",
            behavior.request_context_template.as_deref(),
        ),
        graphql_fields::graphql_string_field("backend_id", behavior.backend_id.as_deref()),
        graphql_fields::graphql_string_field("model_name", behavior.model_name.as_deref()),
        graphql_fields::graphql_string_field(
            "tool_selection_id",
            behavior.tool_selection_id.as_deref(),
        ),
        graphql_fields::graphql_string_field(
            "inference_profile_id",
            behavior.inference_profile_id.as_deref(),
        ),
        graphql_fields::graphql_string_field(
            "compaction_strategy",
            behavior.compaction_strategy.as_deref(),
        ),
        graphql_fields::graphql_optional_float_field(
            "compaction_threshold",
            behavior.compaction_threshold,
        ),
        Some(format!(
            "enabled: {}",
            graphql_fields::graphql_bool(behavior.enabled)
        )),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(",\n                    ");

    format!(
        r#"mutation {{
            upsert_AgentBehavior(
                filter: {{ behavior_id: {{ _eq: "{escaped_behavior_id}" }} }},
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

pub(super) async fn create_default_behavior(
    node: &EmbeddedNode,
    agent_did: &str,
    behavior_id: &str,
    inference_profile_id: &str,
) -> Result<()> {
    upsert_agent_behavior(
        node,
        &AgentBehavior {
            behavior_id: behavior_id.to_string(),
            agent_did: agent_did.to_string(),
            display_name: Some(DEFAULT_BEHAVIOR_LABEL.to_string()),
            description: None,
            summary: None,
            system_prompt: None,
            request_context_template: None,
            backend_id: None,
            model_name: None,
            tool_selection_id: None,
            inference_profile_id: Some(inference_profile_id.to_string()),
            compaction_strategy: None,
            compaction_threshold: None,
            enabled: true,
            skill_refs: Vec::new(),
            skill_excludes: Vec::new(),
            created_at: Some(chrono::Utc::now().to_rfc3339()),
        },
    )
    .await
}
