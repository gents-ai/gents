//! The structured query contract and its translation into a read-only DefraDB
//! GraphQL query.

use anyhow::{anyhow, bail, Result};
use defra_node::EmbeddedNode;
use serde::Deserialize;
use serde_json::Value;

use super::render::{render_filter, validate_identifier};

/// Rows returned when the caller does not specify a `limit`.
pub const DEFAULT_LIMIT: u32 = 50;
/// Hard ceiling on `limit` to keep a single read bounded.
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

/// Recursively collect the field-reference keys in a DefraDB filter object —
/// object keys that are not operators (operators start with `_`, e.g. `_eq`,
/// `_and`). Used to block filtering on restricted fields (which would otherwise
/// allow probing a secret value with boolean/`_like` predicates).
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

/// The structured query contract: `{collection, filter, fields, limit}`.
#[derive(Debug, Clone, Deserialize)]
pub struct DefraQueryParams {
    /// Collection (GraphQL type) name to read, e.g. `AgentRequest`.
    pub collection: String,
    /// Optional DefraDB filter object. `null`/absent means "no filter".
    #[serde(default)]
    pub filter: Option<Value>,
    /// Field names to return. Must be non-empty.
    #[serde(default)]
    pub fields: Vec<String>,
    /// Maximum rows to return (defaults to [`DEFAULT_LIMIT`], capped at [`MAX_LIMIT`]).
    #[serde(default)]
    pub limit: Option<u32>,
}

impl DefraQueryParams {
    /// True when the caller asked for discovery mode (`fields: ["*"]`): return
    /// the collection's queryable field inventory instead of documents.
    pub fn is_discovery(&self) -> bool {
        self.fields.len() == 1 && self.fields[0] == "*"
    }
}

/// Alias accepted in `ToolSelection.defra_query_collections` that expands to
/// [`AGENT_CONFIG_QUERY_COLLECTIONS`]. Lets an operator (or a preset) grant
/// the configuration read surface without enumerating collection names.
pub const AGENT_CONFIG_SCOPE_ALIAS: &str = "agent-config";

/// The configuration read surface: every collection an agent needs to explain
/// its own setup and help diagnose config issues ("how am I configured?",
/// "why doesn't X fire?", "which peers am I paired with?").
///
/// Deliberately excludes conversation content (`AgentRequest`/`AgentResponse`/
/// `AgentMessage`/…), memory, telemetry (`InferenceCall`), and secret-bearing
/// collections (`OAuthCredential`) — this is the agent's operating manual,
/// not its mailbox. `InferenceBackend` rows are included but their key fields
/// are already redacted by the schema's visible-field projection.
pub const AGENT_CONFIG_QUERY_COLLECTIONS: &[&str] = &[
    // Identity + behavior/tool configuration.
    "AgentPrincipal",
    "AgentBehavior",
    "ToolSelection",
    "Skill",
    // Inference configuration (api_key fields are redacted at the schema
    // projection; the raw column never reaches the model).
    "InferenceBackend",
    "InferenceProfile",
    // Tool services + their health, for "why is my MCP tool failing?".
    "ToolServiceRegistry",
    "ToolServiceHealthState",
    // Automation configuration.
    "Task",
    "Schedule",
    "EventTrigger",
    // Runtime reconcile state — the diagnosis anchor (generation, phase).
    "AgentRuntime",
    // Operator-visible P2P control plane, for pairing diagnosis. Addresses
    // here are shareable multiaddrs by design; no key material.
    "AgentNetwork",
    "NetworkMembership",
    "PeerEndpoint",
    "PeerRegistry",
    "PeerPairingDesired",
    "PeerPairingApplied",
    "DataPlanePairingDesired",
];

/// Expand scope aliases in a raw `defra_query_collections` list: each
/// [`AGENT_CONFIG_SCOPE_ALIAS`] entry becomes the full
/// [`AGENT_CONFIG_QUERY_COLLECTIONS`] set; literal collection names pass
/// through unchanged. Callers dedupe via their scope-set types.
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

/// Which collections a query surface is permitted to read.
///
/// An explicit tristate so the deny-all case cannot be confused with allow-all
/// at this projection boundary (the `Only(∅) ≠ All` trap): `None` and an empty
/// `Only` both DENY, only `All` permits everything. `restricted([])` therefore
/// means deny-all, NOT allow-all — callers wanting allow-all must use `all()`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum CollectionScope {
    /// No collection is readable.
    #[default]
    None,
    /// Only the listed collections are readable. An empty set denies everything.
    Only(std::collections::BTreeSet<String>),
    /// Every collection is readable.
    All,
}

impl CollectionScope {
    /// Allow every collection.
    pub fn all() -> Self {
        Self::All
    }

    /// Deny every collection.
    pub fn none() -> Self {
        Self::None
    }

    /// Restrict reads to the given collections. An EMPTY list denies all (it is
    /// `Only(∅)`), never allow-all — use [`CollectionScope::all`] for allow-all.
    pub fn restricted(collections: Vec<String>) -> Self {
        Self::Only(collections.into_iter().collect())
    }

    /// True only when every collection is readable (`All`).
    pub fn is_unrestricted(&self) -> bool {
        matches!(self, Self::All)
    }

    /// Error unless `collection` is readable under this scope.
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

/// Introspect a collection's field set. `Ok(None)` means the collection
/// (GraphQL type) does not exist on the node.
pub(crate) async fn fetch_collection_schema(
    node: &EmbeddedNode,
    collection: &str,
) -> Result<Option<super::schema::CollectionSchema>> {
    let query = super::schema::introspection_query(collection)?;
    let resp = node.execute(&query).await;
    if resp.has_errors() {
        bail!(
            "schema introspection for {collection:?} failed: {:?}",
            resp.errors
        );
    }
    Ok(super::schema::parse_collection_schema(resp.data.as_ref()))
}

/// Execute the structured query against `node` and return the result rows
/// (a JSON array) for the requested collection.
///
/// On a GraphQL failure the collection is introspected and the error is
/// enriched into an agent-usable diagnostic (invalid fields, the allowed
/// inventory, close-match suggestions); if introspection itself fails, the raw
/// GraphQL errors are surfaced unchanged.
pub(crate) async fn execute_query(
    node: &EmbeddedNode,
    params: &DefraQueryParams,
    scope: &CollectionScope,
) -> Result<Value> {
    let query = build_query(params, scope)?;
    let resp = node.execute(&query).await;
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
