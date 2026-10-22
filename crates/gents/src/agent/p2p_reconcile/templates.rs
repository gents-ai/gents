use gents_protocol::peer_schema::{
    compare_replicated_schema, ReplicatedCollectionIdentity, ReplicatedSchema, ReplicatedSchemaSkew,
};
use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

pub use p2p::ReplicationFilter as FilterPredicate;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    Push,
    Replicate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    PeerDid {
        field: &'static str,
    },
    Unscoped,
    PerCollection(&'static [CollectionRule]),
    /// Return-leg runtime configuration cannot be expressed as a DefraDB
    /// replicator predicate because its owner fields are mutable.
    ClientRoute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DidSource {
    LocalDid,
    PeerDid,
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

pub fn combine_filters(left: FilterPredicate, right: FilterPredicate) -> FilterPredicate {
    let mut filters = Vec::new();
    match left {
        FilterPredicate::All(nested) => filters.extend(nested),
        predicate => filters.push(predicate),
    }
    match right {
        FilterPredicate::All(nested) => filters.extend(nested),
        predicate => filters.push(predicate),
    }
    let mut unique = Vec::with_capacity(filters.len());
    for filter in filters {
        if !unique.contains(&filter) {
            unique.push(filter);
        }
    }
    if unique.len() == 1 {
        unique.pop().expect("one filter")
    } else {
        FilterPredicate::All(unique)
    }
}

pub fn filter_conditions(filter: &FilterPredicate) -> Option<Map<String, Value>> {
    match filter {
        FilterPredicate::Predicate(conditions) => Some(conditions.clone()),
        FilterPredicate::All(filters) => {
            let mut conditions = Map::new();
            conditions.insert(
                "_and".to_string(),
                Value::Array(
                    filters
                        .iter()
                        .map(|filter| filter_conditions(filter).map(Value::Object))
                        .collect::<Option<Vec<_>>>()?,
                ),
            );
            Some(conditions)
        }
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

/// The string `field` equals in a live replicator filter, either as its scalar
/// equality or as a top-level or `_and` conjunct of its conditions.
pub fn live_filter_string_eq<'a>(
    filter: &'a defra_p2p_adapter::ReplicationFilter,
    field: &str,
) -> Option<&'a str> {
    fn conjunct<'a>(conditions: &'a Map<String, Value>, field: &str) -> Option<&'a str> {
        conditions
            .get(field)
            .and_then(|condition| condition.get("_eq"))
            .and_then(Value::as_str)
            .or_else(|| {
                conditions
                    .get("_and")?
                    .as_array()?
                    .iter()
                    .filter_map(Value::as_object)
                    .find_map(|nested| conjunct(nested, field))
            })
    }
    match &filter.conditions {
        Some(conditions) => conjunct(conditions, field),
        None => (filter.field == field)
            .then(|| filter.value.as_str())
            .flatten(),
    }
}

pub(super) fn conjunctive_string_eq<'a>(
    filter: &'a FilterPredicate,
    field: &str,
) -> Option<&'a str> {
    fn collect<'a>(filter: &'a FilterPredicate, field: &str, found: &mut Option<&'a str>) -> bool {
        match filter {
            FilterPredicate::Predicate(conditions) => {
                let Some(condition) = conditions.get(field) else {
                    return true;
                };
                let Some(value) = condition
                    .as_object()
                    .and_then(|condition| condition.get("_eq"))
                    .and_then(Value::as_str)
                else {
                    return false;
                };
                match found {
                    Some(existing) => *existing == value,
                    None => {
                        *found = Some(value);
                        true
                    }
                }
            }
            FilterPredicate::All(filters) => {
                filters.iter().all(|filter| collect(filter, field, found))
            }
            FilterPredicate::Acp { .. } => false,
        }
    }

    let mut found = None;
    collect(filter, field, &mut found)
        .then_some(found)
        .flatten()
}

pub type PairingFilters = BTreeMap<String, FilterPredicate>;

pub fn decode_pairing_filters(raw: &str) -> serde_json::Result<PairingFilters> {
    serde_json::from_str(raw)
}

const CONVERSATION_COLLECTIONS: &[&str] = &[
    "AgentRequest",
    "AgentMessage",
    "AgentToolCall",
    "AgentOutputSegment",
    "AgentSession",
    "CompactionEntry",
    "Agent",
    "AgentContext",
    "CompactionConfig",
    "Tools",
    "AgentTarget",
    "InferenceProfile",
    "InferenceSampling",
    "InferenceExecution",
    "InferenceRetryPolicy",
    "ToolServiceRegistry",
    "Skill",
    "DatastoreToolSurface",
    "ChainKeyBinding",
    "EthTool",
];

const CONVERSATION_TRANSCRIPT_COLLECTIONS: &[&str] = &[
    "AgentRequest",
    "AgentMessage",
    "AgentToolCall",
    "AgentOutputSegment",
    "AgentSession",
    "CompactionEntry",
];

// InferenceBackend contains raw API-key material; clients resolve model choices through profiles.
pub const CLIENT_COLLECTIONS: &[&str] = &[
    "AgentRequest",
    "AgentMessage",
    "AgentToolCall",
    "AgentOutputSegment",
    "AgentSession",
    "CompactionEntry",
    "MailboxItem",
    "PeerEndpoint",
    "SessionHydrationRequest",
    "FileImageHydrationRequest",
    "AgentSessionInputEdit",
    "AgentSessionGoalControl",
    "Agent",
    "AgentContext",
    "CompactionConfig",
    "Tools",
    "AgentTarget",
    "InferenceProfile",
    "InferenceSampling",
    "InferenceExecution",
    "InferenceRetryPolicy",
    "ToolServiceRegistry",
    "Skill",
    "DatastoreToolSurface",
    "ChainKeyBinding",
    "EthTool",
    "Task",
    "Schedule",
    "Trigger",
    "EventSource",
    "NodeReadiness",
    "GoalObservation",
];

pub const CLIENT_TO_RUNTIME_COLLECTIONS: &[&str] = &[
    "AgentRequest",
    "AgentMessage",
    "AgentToolCall",
    "AgentOutputSegment",
    "AgentSession",
    "CompactionEntry",
    "MailboxItem",
    "PeerEndpoint",
    "SessionHydrationRequest",
    "FileImageHydrationRequest",
    "AgentSessionInputEdit",
    "AgentSessionGoalControl",
];

const CONVERSATION_RULES: &[CollectionRule] = &[
    CollectionRule {
        collection: "AgentRequest",
        field: "requester_did",
        source: DidSource::PeerDid,
    },
    CollectionRule {
        collection: "AgentMessage",
        field: "requester_did",
        source: DidSource::PeerDid,
    },
    CollectionRule {
        collection: "AgentToolCall",
        field: "requester_did",
        source: DidSource::PeerDid,
    },
    CollectionRule {
        collection: "AgentOutputSegment",
        field: "requester_did",
        source: DidSource::PeerDid,
    },
    CollectionRule {
        collection: "AgentSession",
        field: "requester_did",
        source: DidSource::PeerDid,
    },
    CollectionRule {
        collection: "CompactionEntry",
        field: "requester_did",
        source: DidSource::PeerDid,
    },
];

pub const CLIENT_INDEX_COLLECTIONS: [&str; 2] = ["AgentSession", "MailboxItem"];

const CLIENT_INDEX_RULES: &[CollectionRule] = &[
    CollectionRule {
        collection: "AgentSession",
        field: "requester_did",
        source: DidSource::PeerDid,
    },
    CollectionRule {
        collection: "MailboxItem",
        field: "requester_did",
        source: DidSource::PeerDid,
    },
];

pub const NODE_DIRECTORY_COLLECTION: &str = "NodeDirectoryEntry";

const MACHINE_COLLECTIONS: &[&str] = &[
    "AgentRequest",
    "AgentMessage",
    "AgentToolCall",
    "AgentOutputSegment",
    "AgentSession",
    "CompactionEntry",
    "Agent",
    "AgentContext",
    "CompactionConfig",
    "Tools",
    "AgentTarget",
    "InferenceProfile",
    "InferenceSampling",
    "InferenceExecution",
    "InferenceRetryPolicy",
    "ToolServiceRegistry",
    "Skill",
    "DatastoreToolSurface",
    "ChainKeyBinding",
    "EthTool",
    "MailboxItem",
    "SessionHydrationRequest",
    "AgentSessionInputEdit",
    "AgentSessionGoalControl",
    NODE_DIRECTORY_COLLECTION,
];

const MACHINE_RULES: &[CollectionRule] = &[
    CollectionRule {
        collection: "AgentRequest",
        field: "requester_did",
        source: DidSource::PeerDid,
    },
    CollectionRule {
        collection: "AgentMessage",
        field: "requester_did",
        source: DidSource::PeerDid,
    },
    CollectionRule {
        collection: "AgentToolCall",
        field: "requester_did",
        source: DidSource::PeerDid,
    },
    CollectionRule {
        collection: "AgentOutputSegment",
        field: "requester_did",
        source: DidSource::PeerDid,
    },
    CollectionRule {
        collection: "AgentSession",
        field: "requester_did",
        source: DidSource::PeerDid,
    },
    CollectionRule {
        collection: "CompactionEntry",
        field: "requester_did",
        source: DidSource::PeerDid,
    },
    CollectionRule {
        collection: "MailboxItem",
        field: "requester_did",
        source: DidSource::PeerDid,
    },
    CollectionRule {
        collection: "SessionHydrationRequest",
        field: "requester_did",
        source: DidSource::PeerDid,
    },
    CollectionRule {
        collection: "AgentSessionInputEdit",
        field: "requester_did",
        source: DidSource::PeerDid,
    },
    CollectionRule {
        collection: "AgentSessionGoalControl",
        field: "requester_did",
        source: DidSource::PeerDid,
    },
    CollectionRule {
        collection: NODE_DIRECTORY_COLLECTION,
        field: "source_did",
        source: DidSource::HomeDid,
    },
];

const AGENT_CONFIG_COLLECTIONS: &[&str] = &[
    "Agent",
    "AgentContext",
    "CompactionConfig",
    "Tools",
    "AgentTarget",
    "InferenceProfile",
    "InferenceSampling",
    "InferenceExecution",
    "InferenceRetryPolicy",
    "ToolServiceRegistry",
    "Skill",
    "DatastoreToolSurface",
    "ChainKeyBinding",
    "EthTool",
    "InferenceBackend",
];

const AGENT_TARGET_CALLER_COLLECTIONS: &[&str] = &["AgentRequest"];

const AGENT_TARGET_CALLER_RULES: &[CollectionRule] = &[CollectionRule {
    collection: "AgentRequest",
    field: "node_did",
    source: DidSource::PeerDid,
}];

const AGENT_TARGET_HOST_COLLECTIONS: &[&str] = &[
    "AgentRequest",
    "AgentSession",
    "AgentOutputSegment",
    "AgentMessage",
];

const AGENT_TARGET_HOST_RULES: &[CollectionRule] = &[
    CollectionRule {
        collection: "AgentOutputSegment",
        field: "requester_did",
        source: DidSource::PeerDid,
    },
    CollectionRule {
        collection: "AgentRequest",
        field: "requester_did",
        source: DidSource::PeerDid,
    },
    CollectionRule {
        collection: "AgentSession",
        field: "requester_did",
        source: DidSource::PeerDid,
    },
    CollectionRule {
        collection: "AgentMessage",
        field: "requester_did",
        source: DidSource::PeerDid,
    },
];

pub const AGENT_TARGET_CALLER_TEMPLATE: &str = "agent-target-caller";
pub const AGENT_TARGET_HOST_TEMPLATE: &str = "agent-target-host";
pub const APP_COLLECTIONS_TEMPLATE: &str = "app-collections";
pub const MACHINE_TEMPLATE: &str = "machine";
pub const CLIENT_INDEX_TEMPLATE: &str = "client-index";
pub const CLIENT_TEMPLATE: &str = "client";

static BUILTIN_TEMPLATES: &[ScopeTemplate] = &[
    ScopeTemplate {
        id: "conversation",
        collections: CONVERSATION_COLLECTIONS,
        scope: Scope::PerCollection(CONVERSATION_RULES),
        delivery: Delivery::Push,
    },
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
    ScopeTemplate {
        id: "agent-config",
        collections: AGENT_CONFIG_COLLECTIONS,
        scope: Scope::Unscoped,
        delivery: Delivery::Replicate,
    },
    ScopeTemplate {
        id: "backup",
        collections: CONVERSATION_TRANSCRIPT_COLLECTIONS,
        scope: Scope::Unscoped,
        delivery: Delivery::Replicate,
    },
    ScopeTemplate {
        id: AGENT_TARGET_CALLER_TEMPLATE,
        collections: AGENT_TARGET_CALLER_COLLECTIONS,
        scope: Scope::PerCollection(AGENT_TARGET_CALLER_RULES),
        delivery: Delivery::Push,
    },
    ScopeTemplate {
        id: AGENT_TARGET_HOST_TEMPLATE,
        collections: AGENT_TARGET_HOST_COLLECTIONS,
        scope: Scope::PerCollection(AGENT_TARGET_HOST_RULES),
        delivery: Delivery::Push,
    },
    ScopeTemplate {
        id: APP_COLLECTIONS_TEMPLATE,
        collections: &[],
        scope: Scope::Unscoped,
        delivery: Delivery::Replicate,
    },
    ScopeTemplate {
        id: CLIENT_INDEX_TEMPLATE,
        collections: &CLIENT_INDEX_COLLECTIONS,
        scope: Scope::PerCollection(CLIENT_INDEX_RULES),
        delivery: Delivery::Push,
    },
];

pub fn builtin_templates() -> &'static [ScopeTemplate] {
    BUILTIN_TEMPLATES
}

/// Prevent app routes from bypassing protocol schema compatibility checks.
pub fn admit_app_collections(requested: BTreeSet<String>) -> Option<BTreeSet<String>> {
    let overlaps_protocol_catalog = requested.iter().any(|requested| {
        gents_protocol::schemas::ALL_COLLECTION_NAMES
            .iter()
            .chain(gents_protocol::schemas::RUNTIME_COLLECTION_NAMES.iter())
            .any(|protocol| requested == protocol)
    });
    (!requested.is_empty() && !overlaps_protocol_catalog).then_some(requested)
}

pub async fn read_client_replicated_schema(
    node: std::sync::Arc<defra_node::EmbeddedNode>,
) -> anyhow::Result<ReplicatedSchema> {
    let access = crate::config_client::ConfigAccess::local(node);
    let mut schema = ReplicatedSchema::new();
    for name in CLIENT_COLLECTIONS {
        let identity = access
            .collection_version(name)
            .await?
            .as_ref()
            .and_then(ReplicatedCollectionIdentity::from_collection_version)
            .ok_or_else(|| anyhow::anyhow!("client route collection {name} is not registered"))?;
        schema.insert((*name).to_string(), identity);
    }
    Ok(schema)
}

pub fn compare_client_replicated_schema(
    local: &ReplicatedSchema,
    remote: Option<&ReplicatedSchema>,
) -> Result<(), ReplicatedSchemaSkew> {
    compare_replicated_schema(CLIENT_COLLECTIONS, local, remote)
}

pub fn resolve_template(id: &str) -> Option<&'static ScopeTemplate> {
    BUILTIN_TEMPLATES.iter().find(|t| t.id == id)
}

pub fn scope_filter(
    scope: &Scope,
    collections: &[&str],
    peer_did: &str,
    local_did: &str,
) -> PairingFilters {
    match scope {
        Scope::PeerDid { field } => collections
            .iter()
            .map(|&col| (col.to_string(), equality_filter(*field, peer_did)))
            .collect(),
        Scope::Unscoped => BTreeMap::new(),
        Scope::PerCollection(rules) => rules
            .iter()
            .map(|rule| {
                let value = match rule.source {
                    DidSource::LocalDid => local_did,
                    DidSource::PeerDid => peer_did,
                    DidSource::HomeDid => local_did,
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
mod tests {
    use super::*;

    #[tokio::test]
    async fn pre_reasoning_audit_runtime_schema_is_refused_as_skew() {
        async fn node_with(schemas: &[String]) -> std::sync::Arc<defra_node::EmbeddedNode> {
            let node =
                std::sync::Arc::new(crate::test_support::node_builder().build().await.unwrap());
            node.add_dac_policy_with_counter(
                node.node_identity_did().unwrap(),
                gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY,
                0,
            )
            .await
            .unwrap();
            for sdl in schemas {
                node.add_schema(sdl).await.unwrap();
            }
            node
        }
        let current = gents_protocol::schemas::ALL
            .iter()
            .map(|sdl| (*sdl).to_owned())
            .collect::<Vec<_>>();
        let previous = current
            .iter()
            .map(|sdl| {
                if *sdl == gents_protocol::schemas::AGENT_REQUEST {
                    sdl.replace("    purpose: String @index @immutable\n", "")
                } else {
                    sdl.clone()
                }
            })
            .collect::<Vec<_>>();
        assert_ne!(previous, current, "fixture must revert the purpose field");

        let local = read_client_replicated_schema(node_with(&current).await)
            .await
            .unwrap();
        let same = read_client_replicated_schema(node_with(&current).await)
            .await
            .unwrap();
        assert!(compare_client_replicated_schema(&local, Some(&same)).is_ok());

        let old = read_client_replicated_schema(node_with(&previous).await)
            .await
            .unwrap();
        let skew = compare_client_replicated_schema(&local, Some(&old))
            .expect_err("an old runtime schema must not enroll or sync");
        assert!(format!("{skew:?}").contains("AgentRequest"), "{skew:?}");
    }

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

    #[test]
    fn client_schema_comparison_covers_every_client_route_collection() {
        let identity = |version: &str| ReplicatedCollectionIdentity {
            version_id: version.to_string(),
            branchable: true,
            policy_resource: None,
        };
        let local: ReplicatedSchema = CLIENT_COLLECTIONS
            .iter()
            .map(|name| ((*name).to_string(), identity(name)))
            .collect();
        assert_eq!(
            compare_client_replicated_schema(&local, Some(&local)),
            Ok(())
        );

        for name in CLIENT_COLLECTIONS {
            let mut skewed = local.clone();
            skewed.insert((*name).into(), identity("bafy-other-release"));
            assert_eq!(
                compare_client_replicated_schema(&local, Some(&skewed))
                    .unwrap_err()
                    .collections,
                vec![(*name).to_string()]
            );
        }

        let mut missing = local.clone();
        missing.remove("Task");
        assert_eq!(
            compare_client_replicated_schema(&local, Some(&missing))
                .unwrap_err()
                .collections,
            vec!["Task".to_string()]
        );
    }

    #[tokio::test]
    async fn client_schema_is_unreadable_until_migrations_register_every_collection() {
        let node = std::sync::Arc::new(crate::test_support::node_builder().build().await.unwrap());
        let error = read_client_replicated_schema(node.clone())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("is not registered"), "{error:#}");

        crate::migration::ensure_all_runtime_migrations(node.clone())
            .await
            .unwrap();
        let schema = read_client_replicated_schema(node).await.unwrap();
        assert_eq!(
            schema.keys().map(String::as_str).collect::<BTreeSet<_>>(),
            CLIENT_COLLECTIONS.iter().copied().collect::<BTreeSet<_>>()
        );
    }
}
