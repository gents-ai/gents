//! The id/model sets a config document's reference fields are checked
//! against (#1331). One loader, built from the current node state, feeds
//! every referential validator: `AgentBehavior::validate_references` is the
//! only consumer today, but the shape generalizes to any document that
//! points at a backend, tool selection, profile, or skill.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::Result;
use defra_node::EmbeddedNode;
use gents_protocol::graphql::graphql_rows_from_response;
use serde_json::Value;

use crate::config_client::ConfigAccess;
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
    /// Load the reference sets for `agent_did` from the current node state.
    /// A thin wrapper over [`Self::load_via_access`] — every query this
    /// needs already runs the same way over `ConfigAccess::Local` as over
    /// HTTP, so there is exactly one query path for both, not two to keep
    /// in sync.
    pub async fn load(node: &Arc<EmbeddedNode>, agent_did: &str) -> Result<Self> {
        Self::load_via_access(&ConfigAccess::Local(Arc::clone(node)), agent_did).await
    }

    /// Load the reference sets for `agent_did` via [`ConfigAccess`] (HTTP
    /// GraphQL or local node) — the CLI's raw-write commands (`config
    /// behavior set`, the codex-shim model switch) hold a `ConfigAccess`,
    /// not an `Arc<EmbeddedNode>`, but [`Self::load`] delegates here too so
    /// there is one implementation regardless of caller. Fetches only the
    /// id columns (+ `models`, for backends) a referential-existence check
    /// needs — not a full document decode.
    pub async fn load_via_access(access: &ConfigAccess, agent_did: &str) -> Result<Self> {
        let escaped_agent_did = escape_graphql_string(agent_did);

        // Read directly rather than through
        // `crate::backend_registry::InferenceBackend::from_value`: that
        // parser requires every column (`max_concurrent`, `enabled`, …) an
        // unrelated backend may not have set, and a referential-existence
        // check has no business failing every principal's config
        // validation over one malformed row elsewhere in the collection. A
        // row with no usable `backend_id` is skipped, not fatal.
        let backend_rows = query_rows(
            access,
            "InferenceBackend",
            "{ InferenceBackend { backend_id models } }",
        )
        .await?;
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

        let tool_selection_rows = query_rows(
            access,
            "ToolSelection",
            &format!(
                r#"{{ ToolSelection(filter: {{ agent_did: {{ _eq: "{escaped_agent_did}" }} }}) {{ selection_id }} }}"#
            ),
        )
        .await?;
        let tool_selections = string_field_set(&tool_selection_rows, "selection_id");

        let profile_rows = query_rows(
            access,
            "InferenceProfile",
            "{ InferenceProfile { profile_id } }",
        )
        .await?;
        let profiles = string_field_set(&profile_rows, "profile_id");

        let skill_rows = query_rows(
            access,
            "Skill",
            &format!(
                r#"{{ Skill(filter: {{ agent_did: {{ _eq: "{escaped_agent_did}" }} }}) {{ skill_id }} }}"#
            ),
        )
        .await?;
        let skills = string_field_set(&skill_rows, "skill_id");

        Ok(Self {
            backends,
            tool_selections,
            profiles,
            skills,
        })
    }
}

async fn query_rows(access: &ConfigAccess, collection: &str, query: &str) -> Result<Vec<Value>> {
    let response = access.execute(query).await?;
    Ok(graphql_rows_from_response(&response, collection))
}

fn string_field_set(rows: &[Value], field: &str) -> BTreeSet<String> {
    rows.iter()
        .filter_map(|row| row.get(field)?.as_str().map(str::to_string))
        .collect()
}
