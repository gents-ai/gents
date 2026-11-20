use gents_protocol::peer_schema::{
    decide_app_collection, is_managed_collection, AppCollectionFacts, AppCollectionRefusal,
    ReplicatedCollectionIdentity, ReplicatedSchema,
};
use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

pub use p2p::ReplicationFilter as FilterPredicate;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    Push,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    PerCollection(&'static [CollectionRule]),
    ClientRoute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
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
pub const APP_COLLECTIONS_TEMPLATE: &str = "app-collections";

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
    ScopeTemplate {
        id: APP_COLLECTIONS_TEMPLATE,
        collections: &[],
        scope: Scope::PerCollection(&[]),
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
    let access = gents_store::config_client::ConfigAccess::local(node);
    let mut schema = ReplicatedSchema::new();
    for name in collections {
        let (identity, _) = local_collection_facts(&access, name)
            .await?
            .ok_or_else(|| anyhow::anyhow!("collection {name} is not registered"))?;
        schema.insert((*name).to_string(), identity);
    }
    Ok(schema)
}

async fn local_collection_facts(
    access: &gents_store::config_client::ConfigAccess,
    name: &str,
) -> Result<
    Option<(ReplicatedCollectionIdentity, bool)>,
    gents_store::config_client::ConfigClientError,
> {
    Ok(access.collection_version(name).await?.and_then(|version| {
        let is_view = version.get("Query").is_some_and(|query| !query.is_null());
        ReplicatedCollectionIdentity::from_collection_version(&version)
            .map(|identity| (identity, is_view))
    }))
}

#[derive(Debug, thiserror::Error)]
pub enum AppCollectionAdmissionError {
    #[error("collection {collection:?}: {refusal}")]
    Refused {
        collection: String,
        refusal: AppCollectionRefusal,
    },
    #[error("collection {collection:?} is not registered on this node")]
    NotRegistered { collection: String },
    #[error("collection {collection:?} could not be read")]
    Unreadable {
        collection: String,
        #[source]
        source: gents_store::config_client::ConfigClientError,
    },
}

pub async fn admit_app_collections(
    node: std::sync::Arc<defra_node::Node>,
    requested: &BTreeSet<String>,
) -> Result<ReplicatedSchema, AppCollectionAdmissionError> {
    admit_app_collections_with(
        &gents_store::config_client::ConfigAccess::local(node),
        requested,
    )
    .await
}

pub async fn admit_app_collections_with(
    access: &gents_store::config_client::ConfigAccess,
    requested: &BTreeSet<String>,
) -> Result<ReplicatedSchema, AppCollectionAdmissionError> {
    let mut schema = ReplicatedSchema::new();
    for collection in requested {
        if is_managed_collection(collection) {
            return Err(AppCollectionAdmissionError::Refused {
                collection: collection.clone(),
                refusal: AppCollectionRefusal::Managed,
            });
        }
        let (identity, is_view) = local_collection_facts(access, collection)
            .await
            .map_err(|source| AppCollectionAdmissionError::Unreadable {
                collection: collection.clone(),
                source,
            })?
            .ok_or_else(|| AppCollectionAdmissionError::NotRegistered {
                collection: collection.clone(),
            })?;
        decide_app_collection(AppCollectionFacts {
            managed: false,
            has_policy: identity.policy_resource.is_some(),
            is_view,
        })
        .map_err(|refusal| AppCollectionAdmissionError::Refused {
            collection: collection.clone(),
            refusal,
        })?;
        schema.insert(collection.clone(), identity);
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
    use crate::p2p_reconcile::{
        client_route_collections, resolve_template_filters, PairingDirection,
    };
    use acp::{DocumentACP, DocumentPermission, Identity};
    use identity::Did;
    use std::collections::BTreeSet;
    use std::sync::Arc;

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
        let node = std::sync::Arc::new(
            gents_store::test_support::node_builder()
                .build()
                .await
                .unwrap(),
        );
        gents_store::config_client::ConfigAccess::local(node.clone())
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
        let node = std::sync::Arc::new(
            gents_store::test_support::node_builder()
                .build()
                .await
                .unwrap(),
        );
        let error = read_client_replicated_schema(node.clone())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("is not registered"), "{error:#}");

        gents_store::schema::migration::ensure_all_runtime_migrations(node.clone())
            .await
            .unwrap();
        let schema = read_client_replicated_schema(node).await.unwrap();
        assert_eq!(
            schema.keys().map(String::as_str).collect::<BTreeSet<_>>(),
            CLIENT_COLLECTIONS.iter().copied().collect::<BTreeSet<_>>()
        );
    }

    #[test]
    fn pairing_directions_are_the_modeled_route_directions() {
        gents_lean_vocab::families::scope_templates_route_direction_vocabulary::replay(&[
            PairingDirection::ClientToRuntime,
            PairingDirection::RuntimeToClient,
        ]);
    }

    #[test]
    fn node_route_collections_match_executable_scope_model() {
        gents_lean_vocab::families::scope_node_route_allowed::replay::<PairingDirection>(|input| {
            gents_lean_vocab::families::scope_node_route_allowed::NodeRouteAllowed {
                allowed: client_route_collections(input.direction).contains(&"Node"),
            }
        });
    }

    #[test]
    fn node_route_filters_match_executable_scope_model() {
        let template = resolve_template(CLIENT_TEMPLATE).expect("client template");
        gents_lean_vocab::families::scope_node_route_filter::replay::<PairingDirection>(|input| {
            let filters =
                resolve_template_filters(template, input.direction, &input.requester, &input.owner);
            gents_lean_vocab::families::scope_node_route_filter::NodeRouteFilter {
                node_did: filters
                    .get("Node")
                    .and_then(single_string_eq)
                    .map(|(_, value)| value.to_owned()),
            }
        });
    }

    #[test]
    fn protocol_routes_match_model_actions() {
        use gents_lean_vocab::families::scope_template::TemplateRoute;
        let mut modeled = BTreeSet::new();
        for case in gents_lean_vocab::families::scope_template::cases::<String>() {
            modeled.insert(case.input.id.clone());
            case.check(resolve_template(&case.input.id).map(|template| {
                TemplateRoute {
                    collections: template
                        .collections
                        .iter()
                        .map(|collection| (*collection).to_owned())
                        .collect(),
                    delivery: match template.delivery {
                        Delivery::Push => "push".to_owned(),
                    },
                }
            }));
        }
        assert_eq!(
            builtin_templates()
                .iter()
                .map(|template| template.id.to_owned())
                .collect::<BTreeSet<_>>(),
            modeled
        );
    }

    #[test]
    fn protocol_route_filters_match_model_actions() {
        use gents_lean_vocab::families::scope_template_filters::{
            Clause, Predicate, TemplateFilters,
        };
        gents_lean_vocab::families::scope_template_filters::replay::<PairingDirection>(|input| {
            resolve_template(&input.id).map(|template| TemplateFilters {
                filters: resolve_template_filters(
                    template,
                    input.direction,
                    &input.requester,
                    &input.owner,
                )
                .iter()
                .map(|(collection, filter)| {
                    let FilterPredicate::Predicate(conditions) = filter else {
                        panic!("{collection} route filter is not a predicate");
                    };
                    Predicate {
                        collection: collection.clone(),
                        clauses: conditions
                            .iter()
                            .flat_map(|(field, operators)| {
                                operators.as_object().expect("filter operators").iter().map(
                                    move |(operator, value)| Clause {
                                        field: field.clone(),
                                        operator: operator.clone(),
                                        value: value.as_str().expect("string filter").to_owned(),
                                    },
                                )
                            })
                            .collect(),
                    }
                })
                .collect(),
            })
        });
    }

    fn household_subject(
        subject: &gents_lean_vocab::families::household_policy_trace::TraceSubject,
    ) -> anyhow::Result<acp::Subject> {
        Ok(
            match (
                &subject.actor,
                &subject.resource,
                &subject.id,
                &subject.target_relation,
            ) {
                (Some(actor), _, _, _) => acp::Subject::Entity(Did::new(actor)?),
                (None, Some(resource), Some(id), Some(target_relation)) => {
                    acp::Subject::entity_set(resource, id, target_relation)
                }
                (None, Some(resource), Some(id), None) => {
                    acp::Subject::entity_set(resource, id, "")
                }
                _ => anyhow::bail!("unmodeled relationship subject {subject:?}"),
            },
        )
    }

    async fn household_relationship(
        acp: &acp::ZanzibarDocumentACP<acp::MemoryZanzibarStore>,
        policy_id: &str,
        trace: &gents_lean_vocab::families::household_policy_trace::PolicyTraceInput,
        relation: &gents_lean_vocab::families::household_policy_trace::TraceRelation,
        grant: bool,
    ) -> anyhow::Result<()> {
        use anyhow::Context;
        let object = trace
            .objects
            .iter()
            .find(|object| object.resource == relation.resource && object.id == relation.id)
            .context("modeled relationship has no registered object")?;
        let owner = Did::new(&object.owner)?;
        let subject = household_subject(&relation.subject)?;
        let changed = if grant {
            acp.add_relationship(
                &owner,
                subject,
                policy_id,
                &relation.resource,
                &relation.id,
                &relation.relation,
                &[],
            )
            .await?
        } else {
            acp.delete_relationship(
                &owner,
                subject,
                policy_id,
                &relation.resource,
                &relation.id,
                &relation.relation,
                &[],
            )
            .await?
        };
        anyhow::ensure!(
            changed,
            "{}: modeled relationship action was not applied",
            trace.name
        );
        Ok(())
    }

    async fn household_relationship_as(
        acp: &acp::ZanzibarDocumentACP<acp::MemoryZanzibarStore>,
        policy_id: &str,
        actor: &str,
        relation: &gents_lean_vocab::families::household_policy_trace::TraceRelation,
        grant: bool,
    ) -> anyhow::Result<bool> {
        let actor = Did::new(actor)?;
        let subject = household_subject(&relation.subject)?;
        let result = if grant {
            acp.add_relationship(
                &actor,
                subject,
                policy_id,
                &relation.resource,
                &relation.id,
                &relation.relation,
                &[],
            )
            .await
        } else {
            acp.delete_relationship(
                &actor,
                subject,
                policy_id,
                &relation.resource,
                &relation.id,
                &relation.relation,
                &[],
            )
            .await
        };
        match result {
            Ok(changed) => Ok(changed),
            Err(acp::Error::NotOwner { .. } | acp::Error::NotManager { .. }) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    #[test]
    fn household_policy_is_the_production_policy() {
        gents_lean_vocab::families::household_policy_definition::replay(|_| {
            gents_protocol::schemas::AGENT_OWNERSHIP_POLICY.to_owned()
        });
    }

    #[tokio::test]
    async fn household_policy_registrations_follow_native_policy_ids() -> anyhow::Result<()> {
        use acp::policy_yaml::{build_policy, parse_policy_yaml};
        use acp::ZanzibarStore;
        let policy = build_policy(
            &parse_policy_yaml(gents_protocol::schemas::AGENT_OWNERSHIP_POLICY)
                .map_err(anyhow::Error::msg)?,
            0,
        )?;
        for registration in gents_lean_vocab::families::household_policy_registration::cases() {
            let candidate = build_policy(
                &parse_policy_yaml(&registration.input.candidate_yaml)
                    .map_err(anyhow::Error::msg)?,
                0,
            )?;
            let registry = acp::PersistentZanzibarStore::from_store(Arc::new(
                storage::RegolithStore::in_memory()?,
            ));
            registry.store_policy(&policy).await?;
            let result = registry.store_policy(&candidate).await;
            let retained = registry
                .get_policy(&policy.id)
                .await?
                .expect("registered policy");
            assert_eq!(
                serde_json::to_value(retained)?,
                serde_json::to_value(&policy)?,
                "{}: registration must retain the original definition",
                registration.input.name
            );
            registration.check(
                gents_lean_vocab::families::household_policy_registration::PolicyRegistration {
                    same_id: policy.id == candidate.id,
                    accepted: result.is_ok(),
                },
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn household_policy_actions_drive_native_acp() -> anyhow::Result<()> {
        use acp::{
            policy_yaml::{build_policy, parse_policy_yaml},
            MemoryZanzibarStore, ZanzibarDocumentACP, ZanzibarStore,
        };
        let parsed = parse_policy_yaml(gents_protocol::schemas::AGENT_OWNERSHIP_POLICY)
            .map_err(anyhow::Error::msg)?;
        let policy = build_policy(&parsed, 0)?;
        for case in gents_lean_vocab::families::household_policy_trace::cases() {
            let trace = &case.input;
            let store = Arc::new(MemoryZanzibarStore::new());
            store.store_policy(&policy).await?;
            let acp = ZanzibarDocumentACP::new(Arc::clone(&store));
            let mut engine = acp::PermissionEngine::new(store);
            engine.add_policy(&policy);
            for object in &trace.objects {
                acp.register_doc_object(
                    &Did::new(&object.owner)?,
                    &policy.id,
                    &object.resource,
                    &object.id,
                )
                .await?;
            }
            for relation in &trace.relations {
                household_relationship(&acp, &policy.id, trace, relation, true).await?;
            }
            for enrolled in &trace.enrolled {
                Did::new(enrolled)?;
            }
            let mut outcomes = Vec::new();
            for step in &trace.steps {
                let outcome = match (step.action.as_str(), &step.relation, &step.actor) {
                    ("grant", Some(relation), _) => {
                        household_relationship(&acp, &policy.id, trace, relation, true).await?;
                        None
                    }
                    ("revoke", Some(relation), _) => {
                        household_relationship(&acp, &policy.id, trace, relation, false).await?;
                        None
                    }
                    ("grant_as", Some(relation), Some(actor)) => Some(
                        household_relationship_as(&acp, &policy.id, actor, relation, true).await?,
                    ),
                    ("revoke_as", Some(relation), Some(actor)) => Some(
                        household_relationship_as(&acp, &policy.id, actor, relation, false).await?,
                    ),
                    ("check", _, Some(actor)) => {
                        let (Some(resource), Some(id), Some(permission)) =
                            (&step.resource, &step.id, &step.permission)
                        else {
                            anyhow::bail!("incomplete modeled check {step:?}");
                        };
                        let actor = Did::new(actor)?;
                        Some(match permission.as_str() {
                            "read" | "update" | "delete" => {
                                let native = match permission.as_str() {
                                    "read" => DocumentPermission::Read,
                                    "update" => DocumentPermission::Update,
                                    _ => DocumentPermission::Delete,
                                };
                                acp.check_doc_access(
                                    &Identity::Authenticated(actor),
                                    native,
                                    &policy.id,
                                    resource,
                                    id,
                                )
                                .await?
                            }
                            "invoke" => {
                                engine
                                    .check(&policy.id, resource, id, permission, &actor)
                                    .await?
                            }
                            other => anyhow::bail!("unknown modeled native permission {other}"),
                        })
                    }
                    _ => anyhow::bail!("unmodeled household step {step:?}"),
                };
                outcomes.push(outcome);
            }
            case.check(outcomes);
        }
        Ok(())
    }

    #[test]
    fn builtin_template_rules_are_the_lean_scope_rules() {
        use gents_lean_vocab::families::scope_collection_rules_template_rules::CollectionRule as ModeledRule;

        gents_lean_vocab::families::scope_collection_rules_template_rules::replay::<DidSource>(
            |i| {
                resolve_template(&i.template).and_then(|template| match template.scope {
                    Scope::PerCollection(rules) => Some(
                        rules
                            .iter()
                            .map(|rule| ModeledRule {
                                collection: rule.collection.to_owned(),
                                field: rule.field.to_owned(),
                                source: rule.source,
                            })
                            .collect(),
                    ),
                    Scope::ClientRoute => None,
                })
            },
        );
    }
}
