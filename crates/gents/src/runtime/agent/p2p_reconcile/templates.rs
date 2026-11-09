use gents_protocol::peer_schema::{ReplicatedCollectionIdentity, ReplicatedSchema};
use std::collections::BTreeMap;

use serde_json::{Map, Value};

pub use p2p::ReplicationFilter as FilterPredicate;

/// Every protocol route pushes; no surviving template subscribes or replicates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    Push,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    PerCollection(&'static [CollectionRule]),
    ClientRoute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DidSource {
    LocalDid,
    HomeDid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollectionRule {
    pub collection: &'static str,
    pub field: &'static str,
    pub source: DidSource,
}

#[derive(Debug, Clone)]
pub struct ScopeTemplate {
    pub id: &'static str,
    pub collections: &'static [&'static str],
    pub scope: Scope,
    pub delivery: Delivery,
}

pub fn equality_filter(field: impl Into<String>, value: impl Into<String>) -> FilterPredicate {
    FilterPredicate::eq(field, Value::String(value.into()))
}

/// The conjunction of two predicates as one `_and` condition map, flattening
/// nested conjunctions and dropping repeated conjuncts. Native replication
/// filters have no conjunction variant, and an ACP filter has no condition
/// map, so a conjunction involving one is `None`.
pub fn combine_filters(left: FilterPredicate, right: FilterPredicate) -> Option<FilterPredicate> {
    let mut unique: Vec<Map<String, Value>> = Vec::new();
    for filter in [left, right] {
        let FilterPredicate::Predicate(conditions) = filter else {
            return None;
        };
        for conjunct in conjuncts(conditions) {
            if !unique.contains(&conjunct) {
                unique.push(conjunct);
            }
        }
    }
    Some(if unique.len() == 1 {
        FilterPredicate::Predicate(unique.pop().expect("one filter"))
    } else {
        let mut conditions = Map::new();
        conditions.insert(
            "_and".to_string(),
            Value::Array(unique.into_iter().map(Value::Object).collect()),
        );
        FilterPredicate::Predicate(conditions)
    })
}

fn conjuncts(conditions: Map<String, Value>) -> Vec<Map<String, Value>> {
    if conditions.len() == 1 {
        if let Some(Value::Array(items)) = conditions.get("_and") {
            if let Some(objects) = items
                .iter()
                .map(|item| item.as_object().cloned())
                .collect::<Option<Vec<_>>>()
            {
                return objects;
            }
        }
    }
    vec![conditions]
}

pub fn filter_conditions(filter: &FilterPredicate) -> Option<Map<String, Value>> {
    match filter {
        FilterPredicate::Predicate(conditions) => Some(conditions.clone()),
        FilterPredicate::Acp { .. } => None,
    }
}

pub fn to_replication_filters(
    filters: &PairingFilters,
) -> Result<defra_p2p_adapter::ReplicationFilters, String> {
    filters
        .iter()
        .map(|(collection, filter)| {
            let conditions = filter_conditions(filter).ok_or_else(|| {
                format!("ACP filter for {collection} is not supported by the replication API")
            })?;
            Ok((
                collection.clone(),
                defra_p2p_adapter::ReplicationFilter::predicate(conditions),
            ))
        })
        .collect()
}

pub fn single_string_eq(filter: &FilterPredicate) -> Option<(&str, &str)> {
    let FilterPredicate::Predicate(conditions) = filter else {
        return None;
    };
    let (field, value) = conditions.iter().next()?;
    if conditions.len() != 1 {
        return None;
    }
    let operators = value.as_object()?;
    let value = operators.get("_eq")?.as_str()?;
    (operators.len() == 1).then_some((field.as_str(), value))
}

pub type PairingFilters = BTreeMap<String, FilterPredicate>;

pub fn decode_pairing_filters(raw: &str) -> serde_json::Result<PairingFilters> {
    serde_json::from_str(raw)
}

pub const CLIENT_COLLECTIONS: &[&str] = &["PeerRegistry"];
pub const CLIENT_TO_RUNTIME_COLLECTIONS: &[&str] = CLIENT_COLLECTIONS;
pub const NODE_DIRECTORY_COLLECTION: &str = "NodeDirectoryEntry";

const MACHINE_COLLECTIONS: &[&str] = &[NODE_DIRECTORY_COLLECTION];
const MACHINE_RULES: &[CollectionRule] = &[CollectionRule {
    collection: NODE_DIRECTORY_COLLECTION,
    field: "source_did",
    source: DidSource::HomeDid,
}];

pub const MACHINE_TEMPLATE: &str = "machine";
pub const CLIENT_TEMPLATE: &str = "client";

static BUILTIN_TEMPLATES: &[ScopeTemplate] = &[
    ScopeTemplate {
        id: MACHINE_TEMPLATE,
        collections: MACHINE_COLLECTIONS,
        scope: Scope::PerCollection(MACHINE_RULES),
        delivery: Delivery::Push,
    },
    ScopeTemplate {
        id: CLIENT_TEMPLATE,
        collections: CLIENT_COLLECTIONS,
        scope: Scope::ClientRoute,
        delivery: Delivery::Push,
    },
];

pub fn builtin_templates() -> &'static [ScopeTemplate] {
    BUILTIN_TEMPLATES
}

pub async fn read_client_replicated_schema(
    node: std::sync::Arc<defra_node::Node>,
) -> anyhow::Result<ReplicatedSchema> {
    read_replicated_schema(node, CLIENT_COLLECTIONS).await
}

pub async fn read_replicated_schema(
    node: std::sync::Arc<defra_node::Node>,
    collections: &[&str],
) -> anyhow::Result<ReplicatedSchema> {
    let access = crate::store::config_client::ConfigAccess::local(node);
    let mut schema = ReplicatedSchema::new();
    for name in collections {
        let identity = access
            .collection_version(name)
            .await?
            .as_ref()
            .and_then(ReplicatedCollectionIdentity::from_collection_version)
            .ok_or_else(|| anyhow::anyhow!("collection {name} is not registered"))?;
        schema.insert((*name).to_string(), identity);
    }
    Ok(schema)
}

pub fn resolve_template(id: &str) -> Option<&'static ScopeTemplate> {
    BUILTIN_TEMPLATES.iter().find(|t| t.id == id)
}

pub fn scope_filter(scope: &Scope, local_did: &str) -> PairingFilters {
    match scope {
        Scope::PerCollection(rules) => rules
            .iter()
            .map(|rule| {
                let value = match rule.source {
                    DidSource::LocalDid | DidSource::HomeDid => local_did,
                };
                (
                    rule.collection.to_string(),
                    equality_filter(rule.field, value),
                )
            })
            .collect(),
        Scope::ClientRoute => panic!(
            "client-route scope requires an explicit direction; use resolve_template_filters"
        ),
    }
}

#[cfg(test)]
mod file_route_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn unsupported_acp_filter_is_rejected() {
        let filters = [(
            "AgentRequest".to_string(),
            FilterPredicate::Acp {
                relation: "reader".to_string(),
            },
        )]
        .into_iter()
        .collect();

        assert!(to_replication_filters(&filters).is_err());
    }

    #[tokio::test]
    async fn requested_app_schema_is_local_and_fails_for_missing_collections() {
        let node = std::sync::Arc::new(crate::test_support::node_builder().build().await.unwrap());
        crate::store::config_client::ConfigAccess::local(node.clone())
            .add_collection("type GameState @branchable { turn: String }")
            .await
            .unwrap();
        let schema = read_replicated_schema(node.clone(), &["GameState"])
            .await
            .unwrap();
        assert_eq!(schema.len(), 1);
        assert!(schema["GameState"].branchable);
        assert!(!schema["GameState"].version_id.is_empty());
        let error = read_replicated_schema(node, &["GameState", "MissingGame"])
            .await
            .unwrap_err();
        assert!(error.to_string().contains("MissingGame"));
    }

    #[tokio::test]
    async fn client_schema_is_unreadable_until_migrations_register_every_collection() {
        let node = std::sync::Arc::new(crate::test_support::node_builder().build().await.unwrap());
        let error = read_client_replicated_schema(node.clone())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("is not registered"), "{error:#}");

        crate::store::schema::migration::ensure_all_runtime_migrations(node.clone())
            .await
            .unwrap();
        let schema = read_client_replicated_schema(node).await.unwrap();
        assert_eq!(
            schema.keys().map(String::as_str).collect::<BTreeSet<_>>(),
            CLIENT_COLLECTIONS.iter().copied().collect::<BTreeSet<_>>()
        );
    }
}
