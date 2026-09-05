//! The id/model sets a config document's reference fields are checked
//! against (#1331). One transactional loader feeds
//! every referential validator: `AgentBehavior::validate_references` is the
//! only consumer today, but the shape generalizes to any document that
//! points at a backend, tool selection, profile, or skill.

use anyhow::Result;
use gents_protocol::graphql::graphql_rows_from_response;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

use crate::config_client::ConfigApplyTxn;
use crate::graphql::escape_graphql_string;

/// Snapshot of the config documents a referencing document (today, only
/// `AgentBehavior`) may point at. Backends and profiles are global by
/// design (see `self_config::backend_request`/`profile_request`); tool
/// selections and skills are scoped to the referencing document's own
/// principal.
#[derive(Debug, Clone, Default)]
pub struct ConfigReferences {
    /// `backend_id` -> the models it advertises (empty means "any model is
    /// accepted", matching `InferenceBackend`'s own no-lockout semantics).
    pub backends: BTreeMap<String, Vec<String>>,
    pub tool_selections: BTreeSet<String>,
    pub profiles: BTreeSet<String>,
    pub skills: BTreeSet<String>,
}

impl ConfigReferences {
    /// Load the same reference snapshot through an already-open config
    /// transaction. Behavior validation must share the transaction's read set
    /// with its eventual write so a concurrent delete cannot make a previously
    /// validated reference dangle at commit time.
    pub async fn load_in_txn(txn: &ConfigApplyTxn<'_>, agent_did: &str) -> Result<Self> {
        Self::from_row_sets(
            query_rows_in_txn(
                txn,
                "InferenceBackend",
                "{ InferenceBackend { backend_id models } }",
            )
            .await?,
            query_rows_in_txn(txn, "ToolSelection", &tool_selection_query(agent_did)).await?,
            query_rows_in_txn(
                txn,
                "InferenceProfile",
                "{ InferenceProfile { profile_id } }",
            )
            .await?,
            query_rows_in_txn(txn, "Skill", &skill_query(agent_did)).await?,
        )
    }

    fn from_row_sets(
        backend_rows: Vec<Value>,
        tool_selection_rows: Vec<Value>,
        profile_rows: Vec<Value>,
        skill_rows: Vec<Value>,
    ) -> Result<Self> {
        // Read directly rather than through
        // `crate::backend_registry::InferenceBackend::from_value`: that
        // parser requires every column (`max_concurrent`, `enabled`, …) an
        // unrelated backend may not have set, and a referential-existence
        // check has no business failing every principal's config
        // validation over one malformed row elsewhere in the collection. A
        // row with no usable `backend_id` is skipped, not fatal.
        let backends = backend_rows
            .into_iter()
            .filter_map(|row| {
                let backend_id = row.get("backend_id")?.as_str()?.to_string();
                let models = row
                    .get("models")
                    .and_then(Value::as_array)
                    .map(|models| {
                        models
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                Some((backend_id, models))
            })
            .collect();

        let tool_selections = string_field_set(&tool_selection_rows, "selection_id");
        let profiles = string_field_set(&profile_rows, "profile_id");
        let skills = string_field_set(&skill_rows, "skill_id");

        Ok(Self {
            backends,
            tool_selections,
            profiles,
            skills,
        })
    }
}

async fn query_rows_in_txn(
    txn: &ConfigApplyTxn<'_>,
    collection: &str,
    query: &str,
) -> Result<Vec<Value>> {
    let response = txn.execute(query).await?;
    Ok(graphql_rows_from_response(&response, collection))
}

fn tool_selection_query(agent_did: &str) -> String {
    let agent_did = escape_graphql_string(agent_did);
    format!(
        r#"{{ ToolSelection(filter: {{ agent_did: {{ _eq: "{agent_did}" }} }}) {{ selection_id }} }}"#
    )
}

fn skill_query(agent_did: &str) -> String {
    let agent_did = escape_graphql_string(agent_did);
    format!(r#"{{ Skill(filter: {{ agent_did: {{ _eq: "{agent_did}" }} }}) {{ skill_id }} }}"#)
}

fn string_field_set(rows: &[Value], field: &str) -> BTreeSet<String> {
    rows.iter()
        .filter_map(|row| row.get(field)?.as_str().map(str::to_string))
        .collect()
}
