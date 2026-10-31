use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use serde_json::Value;

use super::render::{render_filter, validate_identifier};

pub const DEFAULT_LIMIT: u32 = 50;
/// Caps database rows per query to bound result allocation and provider input.
pub const MAX_LIMIT: u32 = 1000;

// The node holds the store's at-rest key, so encryption protects only the
// disk and its backups, never a query this node answers; these credential
// fields are never selected, filtered or listed regardless of scope.
pub(crate) const RESTRICTED_FIELDS: &[(&str, &str)] = &[
    ("InferenceBackend", "auth"),
    ("OAuthCredential", "access_token"),
    ("OAuthCredential", "refresh_token"),
    ("OAuthCredential", "id_token"),
];

pub fn is_restricted_field(collection: &str, field: &str) -> bool {
    RESTRICTED_FIELDS
        .iter()
        .any(|(c, f)| *c == collection && *f == field)
}

pub(crate) fn collect_filter_field_keys(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, nested) in map {
                if !key.starts_with('_') {
                    out.push(key.clone());
                }
                collect_filter_field_keys(nested, out);
            }
        }
        Value::Array(items) => items
            .iter()
            .for_each(|item| collect_filter_field_keys(item, out)),
        _ => {}
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct DefraQueryParams {
    pub collection: String,
    #[serde(default)]
    pub filter: Option<Value>,
    #[serde(default)]
    pub fields: Vec<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

impl DefraQueryParams {
    pub fn is_discovery(&self) -> bool {
        self.fields.len() == 1 && self.fields[0] == "*"
    }
}

pub const AGENT_CONFIG_SCOPE_ALIAS: &str = "agent-config";

pub const AGENT_CONFIG_QUERY_COLLECTIONS: &[&str] = &[
    "Node",
    "Agent",
    "AgentContext",
    "CompactionConfig",
    "Tools",
    "AgentTarget",
    "Skill",
    "DatastoreToolSurface",
    "InferenceBackend",
    "InferenceProfile",
    "InferenceSampling",
    "InferenceExecution",
    "InferenceRetryPolicy",
    "ToolServiceRegistry",
    "ToolServiceHealthState",
    "Task",
    "Schedule",
    "EventSource",
    "Trigger",
    "NodeRuntime",
    "Network",
    "PeerEndpoint",
    "PeerRegistry",
    "PeerPairingDesired",
    "PeerPairingApplied",
    "DataPlanePairingDesired",
];

pub fn expand_collection_scope_aliases<'a>(
    collections: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    let mut expanded = Vec::new();
    for entry in collections {
        let entry = entry.trim();
        if entry == AGENT_CONFIG_SCOPE_ALIAS {
            expanded.extend(
                AGENT_CONFIG_QUERY_COLLECTIONS
                    .iter()
                    .map(|collection| collection.to_string()),
            );
        } else if !entry.is_empty() {
            expanded.push(entry.to_string());
        }
    }
    expanded
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum CollectionScope {
    #[default]
    None,
    Only(std::collections::BTreeSet<String>),
    All,
}

impl CollectionScope {
    pub fn all() -> Self {
        Self::All
    }

    pub fn none() -> Self {
        Self::None
    }

    pub fn restricted(collections: Vec<String>) -> Self {
        Self::Only(collections.into_iter().collect())
    }

    pub fn is_unrestricted(&self) -> bool {
        matches!(self, Self::All)
    }

    pub fn ensure_allowed(&self, collection: &str) -> Result<()> {
        crate::documents::reject_protected_collection_name(collection)?;
        match self {
            Self::All => Ok(()),
            Self::Only(allowed) if allowed.contains(collection) => Ok(()),
            Self::None => bail!(
                "collection {collection:?} is not within the allowed query scope: [] (deny-all)"
            ),
            Self::Only(allowed) => bail!(
                "collection {collection:?} is not within the allowed query scope: [{}]",
                allowed.iter().cloned().collect::<Vec<_>>().join(", ")
            ),
        }
    }
}

pub fn build_query(params: &DefraQueryParams, scope: &CollectionScope) -> Result<String> {
    validate_identifier(&params.collection).map_err(|e| anyhow!("invalid collection name: {e}"))?;
    scope.ensure_allowed(&params.collection)?;

    if params.fields.is_empty() {
        bail!("`fields` must list at least one field to return");
    }
    if params.fields.iter().any(|f| f == "*") {
        bail!(
            "wildcard field \"*\" must be the only entry: call with fields: [\"*\"] \
             to list the collection's queryable fields"
        );
    }
    for field in &params.fields {
        validate_identifier(field).map_err(|e| anyhow!("invalid field name: {e}"))?;
        if is_restricted_field(&params.collection, field) {
            bail!(
                "field {field:?} on {:?} is restricted and cannot be queried",
                params.collection
            );
        }
    }

    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);

    let mut args = Vec::new();
    if let Some(filter) = params.filter.as_ref().filter(|f| !f.is_null()) {
        let mut filter_fields = Vec::new();
        collect_filter_field_keys(filter, &mut filter_fields);
        for field in &filter_fields {
            if is_restricted_field(&params.collection, field) {
                bail!(
                    "field {field:?} on {:?} is restricted and cannot be used in a filter",
                    params.collection
                );
            }
        }
        let rendered = render_filter(filter)?;
        if rendered != "{}" {
            args.push(format!("filter: {rendered}"));
        }
    }
    args.push(format!("limit: {limit}"));

    Ok(format!(
        "{{ {collection}({args}) {{ {fields} }} }}",
        collection = params.collection,
        args = args.join(", "),
        fields = params.fields.join(" "),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn params(collection: &str, fields: &[&str]) -> DefraQueryParams {
        DefraQueryParams {
            collection: collection.to_string(),
            filter: None,
            fields: fields.iter().map(|f| f.to_string()).collect(),
            limit: None,
        }
    }

    #[test]
    fn no_scope_grants_a_protected_collection() {
        for name in crate::documents::PROTECTED_DATASTORE_COLLECTIONS {
            for scope in [
                CollectionScope::all(),
                CollectionScope::restricted(vec![name.to_string()]),
            ] {
                let err = build_query(&params(name, &["x"]), &scope).unwrap_err();
                let message = format!("{err:#}");
                assert!(message.contains("protected"), "{message}");
                assert!(message.contains(name), "{message}");
            }
        }
    }

    #[test]
    fn deny_all_scopes_reject_every_collection() {
        for scope in [
            CollectionScope::none(),
            CollectionScope::restricted(Vec::new()),
        ] {
            assert!(!scope.is_unrestricted());
            let err = build_query(&params("AnythingGoes", &["x"]), &scope).unwrap_err();
            assert!(
                err.to_string()
                    .contains("not within the allowed query scope"),
                "deny-all scope must reject every collection, got: {err}"
            );
        }
    }

    #[test]
    fn rejects_selecting_a_restricted_secret_field() {
        let err = build_query(
            &params("InferenceBackend", &["backend_id", "auth"]),
            &CollectionScope::all(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("restricted"), "{err}");
    }

    #[test]
    fn rejects_restricted_field_nested_in_filter_composition() {
        let mut p = params("InferenceBackend", &["backend_id"]);
        p.filter = Some(json!({ "_or": [{ "auth": { "_ne": null } }] }));
        let err = build_query(&p, &CollectionScope::all()).unwrap_err();
        assert!(err.to_string().contains("restricted"), "{err}");
    }

    #[test]
    fn every_restricted_field_names_a_current_sdl_field() {
        for (collection, field) in RESTRICTED_FIELDS {
            assert!(
                gents_protocol::schemas::is_credential_collection(collection),
                "{collection} is not a credential collection"
            );
            let sdl = match *collection {
                "InferenceBackend" => gents_protocol::schemas::INFERENCE_BACKEND,
                "OAuthCredential" => gents_protocol::schemas::OAUTH_CREDENTIAL,
                other => panic!("restricted field on unexpected collection {other}"),
            };
            assert!(
                sdl.lines()
                    .any(|line| line.trim_start().starts_with(&format!("{field}:"))),
                "{collection}.{field} is not a field of the current SDL"
            );
        }
    }
}
