//! The structured query contract and its translation into a read-only DefraDB
//! GraphQL query.

use anyhow::{anyhow, bail, Result};
use defra_node::EmbeddedNode;
use serde::Deserialize;
use serde_json::Value;

use super::render::{render_filter, validate_identifier};
use crate::graphql::{graphql_response_with_transaction_retry, graphql_with_transaction_retry};

pub const DEFAULT_LIMIT: u32 = 50;
pub const MAX_LIMIT: u32 = 1000;

/// Sensitive `(collection, field)` pairs that `defra_query` must never expose,
/// regardless of the configured collection scope. Selecting or filtering on one
/// of these is rejected — this is an always-on guard against leaking
/// credentials (e.g. inference backend API keys) through the read surface.
const RESTRICTED_FIELDS: &[(&str, &str)] = &[
    ("InferenceBackend", "api_key"),
    ("InferenceBackend", "api_key_env_var"),
    ("OAuthCredential", "access_token"),
    ("OAuthCredential", "refresh_token"),
    ("OAuthCredential", "id_token"),
];

pub(crate) fn is_restricted_field(collection: &str, field: &str) -> bool {
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
    /// Optional DefraDB filter object. `null`/absent means "no filter".
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
    "AgentPrincipal",
    "AgentBehavior",
    "ToolSelection",
    "Skill",
    "DatastoreToolSurface",
    "InferenceBackend",
    "InferenceProfile",
    "ToolServiceRegistry",
    "ToolServiceHealthState",
    "Task",
    "Schedule",
    "EventTrigger",
    "AgentRuntime",
    "AgentNetwork",
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

/// Build the read-only GraphQL query string from the structured contract.
///
/// The collection name and every field name are validated as identifiers, and
/// the filter is rendered through [`render_filter`] (which escapes all string
/// literals), so untrusted input cannot inject GraphQL.
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

pub(crate) async fn fetch_collection_schema(
    node: &EmbeddedNode,
    collection: &str,
) -> Result<Option<super::schema::CollectionSchema>> {
    let query = super::schema::introspection_query(collection)?;
    let operation = format!("schema introspection for {collection:?}");
    let resp = graphql_with_transaction_retry(node, &query, &operation).await?;
    Ok(super::schema::parse_collection_schema(resp.data.as_ref()))
}

pub(crate) async fn execute_query(
    node: &EmbeddedNode,
    params: &DefraQueryParams,
    scope: &CollectionScope,
) -> Result<Value> {
    let query = build_query(params, scope)?;
    // Keep the raw response here because this tool enriches DefraDB's error
    // with a collection-schema diagnostic before returning it to the agent.
    let resp = graphql_response_with_transaction_retry(node, &query, "defra_query").await;
    if resp.has_errors() {
        let raw = format!("{:?}", resp.errors);
        let diagnostic = match fetch_collection_schema(node, &params.collection).await {
            Ok(schema) => super::schema::diagnose_failed_query(params, schema.as_ref(), &raw),
            Err(_) => raw,
        };
        bail!(
            "defra_query against {:?} failed: {diagnostic}",
            params.collection
        );
    }
    let rows = resp
        .data
        .as_ref()
        .and_then(|data| data.get(&params.collection))
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    Ok(rows)
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
    fn agent_config_alias_expands_to_the_preset() {
        let expanded = expand_collection_scope_aliases(["agent-config"]);
        assert_eq!(
            expanded,
            AGENT_CONFIG_QUERY_COLLECTIONS
                .iter()
                .map(|c| c.to_string())
                .collect::<Vec<_>>()
        );
        // The preset is config-only: no conversation content, no secrets.
        for excluded in [
            "AgentRequest",
            "AgentResponse",
            "AgentMessage",
            "OAuthCredential",
        ] {
            assert!(!expanded.contains(&excluded.to_string()), "{excluded}");
        }
    }

    #[test]
    fn literal_collections_pass_through_alias_expansion() {
        let expanded =
            expand_collection_scope_aliases(["AgentRequest", " agent-config ", "", "Custom"]);
        assert!(expanded.contains(&"AgentRequest".to_string()));
        assert!(expanded.contains(&"Custom".to_string()));
        assert!(
            expanded.contains(&"AgentBehavior".to_string()),
            "alias expanded"
        );
        assert!(!expanded.contains(&String::new()), "empties dropped");
        assert!(
            !expanded.contains(&"agent-config".to_string()),
            "the alias itself never survives as a literal"
        );
    }

    #[test]
    fn builds_filtered_query_with_explicit_limit() {
        let mut p = params("AgentRequest", &["request_id", "status"]);
        p.filter = Some(json!({ "status": { "_eq": "pending" } }));
        p.limit = Some(10);

        let query = build_query(&p, &CollectionScope::all()).unwrap();
        assert_eq!(
            query,
            r#"{ AgentRequest(filter: { status: { _eq: "pending" } }, limit: 10) { request_id status } }"#
        );
    }

    #[test]
    fn defaults_limit_when_unset_and_omits_empty_filter() {
        let query = build_query(
            &params("AgentSession", &["session_id"]),
            &CollectionScope::all(),
        )
        .unwrap();
        assert_eq!(query, "{ AgentSession(limit: 50) { session_id } }");
    }

    #[test]
    fn clamps_limit_to_max() {
        let mut p = params("AgentRequest", &["request_id"]);
        p.limit = Some(99_999);
        let query = build_query(&p, &CollectionScope::all()).unwrap();
        assert_eq!(query, "{ AgentRequest(limit: 1000) { request_id } }");
    }

    #[test]
    fn rejects_empty_fields() {
        let err = build_query(&params("AgentRequest", &[]), &CollectionScope::all()).unwrap_err();
        assert!(err.to_string().contains("at least one field"), "{err}");
    }

    #[test]
    fn rejects_collection_outside_scope() {
        let scope = CollectionScope::restricted(vec!["AgentRequest".to_string()]);
        let err = build_query(&params("InferenceBackend", &["backend_id"]), &scope).unwrap_err();
        assert!(
            err.to_string()
                .contains("not within the allowed query scope"),
            "{err}"
        );
    }

    #[test]
    fn allows_any_collection_when_unrestricted() {
        let scope = CollectionScope::all();
        assert!(build_query(&params("AnythingGoes", &["x"]), &scope).is_ok());
    }

    #[test]
    fn deny_all_scopes_reject_every_collection() {
        // The `Only(∅) ≠ All` trap: an empty allowlist and `None` both DENY,
        // never allow-all. `restricted([])` must NOT behave like `all()`.
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
            &params("InferenceBackend", &["backend_id", "api_key"]),
            &CollectionScope::all(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("restricted"), "{err}");
    }

    #[test]
    fn rejects_selecting_oauth_token_fields() {
        let err = build_query(
            &params("OAuthCredential", &["credential_id", "refresh_token"]),
            &CollectionScope::all(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("restricted"), "{err}");
    }

    #[test]
    fn allows_non_secret_fields_on_a_sensitive_collection() {
        assert!(build_query(
            &params(
                "InferenceBackend",
                &["backend_id", "endpoint", "provider_kind"]
            ),
            &CollectionScope::all()
        )
        .is_ok());
    }

    #[test]
    fn rejects_filtering_on_a_restricted_secret_field() {
        let mut p = params("InferenceBackend", &["backend_id"]);
        p.filter = Some(json!({ "api_key": { "_like": "sk-%" } }));
        let err = build_query(&p, &CollectionScope::all()).unwrap_err();
        assert!(err.to_string().contains("restricted"), "{err}");
    }

    #[test]
    fn rejects_restricted_field_nested_in_filter_composition() {
        let mut p = params("InferenceBackend", &["backend_id"]);
        p.filter = Some(json!({ "_or": [{ "api_key_env_var": { "_eq": "X" } }] }));
        let err = build_query(&p, &CollectionScope::all()).unwrap_err();
        assert!(err.to_string().contains("restricted"), "{err}");
    }

    #[test]
    fn rejects_filtering_on_oauth_token_fields() {
        let mut p = params("OAuthCredential", &["credential_id"]);
        p.filter = Some(json!({ "_or": [{ "access_token": { "_like": "eyJ%" } }] }));
        let err = build_query(&p, &CollectionScope::all()).unwrap_err();
        assert!(err.to_string().contains("restricted"), "{err}");
    }
}
