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
    let access = gents_store::config_client::ConfigAccess::local(node);
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
        gents_lean_vocab::families::scope_node_route_allowed::replay::<PairingDirection>(
            |input| gents_lean_vocab::families::scope_node_route_allowed::NodeRouteAllowed {
                allowed: client_route_collections(input.direction).contains(&"Node"),
            },
        );
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
            case.check(
                resolve_template(&case.input.id).map(|template| TemplateRoute {
                    collections: template
                        .collections
                        .iter()
                        .map(|collection| (*collection).to_owned())
                        .collect(),
                    delivery: match template.delivery {
                        Delivery::Push => "push".to_owned(),
                    },
                }),
            );
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
                                operators
                                    .as_object()
                                    .expect("filter operators")
                                    .iter()
                                    .map(move |(operator, value)| Clause {
                                        field: field.clone(),
                                        operator: operator.clone(),
                                        value: value.as_str().expect("string filter").to_owned(),
                                    })
                            })
                            .collect(),
                    }
                })
                .collect(),
            })
        });
    }

    fn household_subject(
        subject: &gents_lean_vocab::LeanIdentitySubject,
    ) -> anyhow::Result<acp::Subject> {
        use gents_lean_vocab::LeanIdentitySubject;
        Ok(match subject {
            LeanIdentitySubject::Actor { actor } => acp::Subject::Entity(Did::new(actor)?),
            LeanIdentitySubject::Object { resource, id } => {
                acp::Subject::entity_set(resource, id, "")
            }
            LeanIdentitySubject::EntitySet {
                resource,
                id,
                target_relation,
            } => acp::Subject::entity_set(resource, id, target_relation),
        })
    }

    async fn household_relationship(
        acp: &acp::ZanzibarDocumentACP<acp::MemoryZanzibarStore>,
        policy_id: &str,
        case: &gents_lean_vocab::LeanIdentityHouseholdCase,
        relation: &gents_lean_vocab::LeanIdentityRelation,
        grant: bool,
    ) -> anyhow::Result<()> {
        use anyhow::Context;
        let object = case
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
            case.name
        );
        Ok(())
    }

    async fn household_relationship_as(
        acp: &acp::ZanzibarDocumentACP<acp::MemoryZanzibarStore>,
        policy_id: &str,
        actor: &str,
        relation: &gents_lean_vocab::LeanIdentityRelation,
        grant: bool,
        expected: bool,
    ) -> anyhow::Result<()> {
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
            Ok(changed) => {
                anyhow::ensure!(
                    expected && changed,
                    "unexpected relationship mutation by {actor}"
                );
            }
            Err(acp::Error::NotOwner { .. } | acp::Error::NotManager { .. }) if !expected => (),
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    #[tokio::test]
    async fn household_policy_actions_drive_native_acp() -> anyhow::Result<()> {
        use acp::{
            policy_yaml::{build_policy, parse_policy_yaml},
            MemoryZanzibarStore, ZanzibarDocumentACP, ZanzibarStore,
        };
        use gents_lean_vocab::{lean_identity_household_cases, LeanIdentityAction};
        for contract in gents_lean_vocab::lean_identity_contracts() {
            assert_eq!(
                contract.schema_actor, contract.runtime_actor,
                "schema installation and updates use the modeled runtime actor"
            );
            for collection in &contract.node_local_collections {
                let schema = match collection.as_str() {
                    "Agent" => gents_protocol::schemas::AGENT,
                    "CallbackBinding" => gents_protocol::schemas::CALLBACK_BINDING,
                    "CallbackInvocation" => gents_protocol::schemas::CALLBACK_INVOCATION,
                    "CallbackResult" => gents_protocol::schemas::CALLBACK_RESULT,
                    other => anyhow::bail!("unknown modeled local collection {other}"),
                };
                assert!(
                    !schema.contains("@branchable"),
                    "{collection} must remain local"
                );
                assert!(gents_protocol::schemas::is_local_only_collection(
                    collection
                ));
                for template in builtin_templates() {
                    assert!(
                        !template.collections.contains(&collection.as_str()),
                        "{} must not replicate {collection}",
                        template.id
                    );
                }
            }
        }
        let cases = lean_identity_household_cases();
        assert!(!cases.is_empty());
        for case in cases {
            assert_eq!(
                case.policy_yaml,
                gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
                "the policy wired to AgentSession must match the executable household model"
            );
            let parsed = parse_policy_yaml(&case.policy_yaml).map_err(anyhow::Error::msg)?;
            let policy = build_policy(&parsed, 0)?;
            for registration in &case.policy_registrations {
                let candidate = build_policy(
                    &parse_policy_yaml(&registration.candidate_yaml).map_err(anyhow::Error::msg)?,
                    0,
                )?;
                assert_eq!(
                    policy.id == candidate.id,
                    registration.same_id,
                    "{}: policy ID",
                    registration.name
                );
                let registry = acp::PersistentZanzibarStore::from_store(Arc::new(
                    storage::RegolithStore::in_memory()?,
                ));
                registry.store_policy(&policy).await?;
                let result = registry.store_policy(&candidate).await;
                assert_eq!(
                    result.is_ok(),
                    registration.accepted,
                    "{}: {result:?}",
                    registration.name
                );
                let retained = registry
                    .get_policy(&policy.id)
                    .await?
                    .expect("registered policy");
                assert_eq!(
                    serde_json::to_value(retained)?,
                    serde_json::to_value(&policy)?,
                    "{}: registration must retain the original definition",
                    registration.name
                );
            }
            let store = Arc::new(MemoryZanzibarStore::new());
            store.store_policy(&policy).await?;
            let acp = ZanzibarDocumentACP::new(Arc::clone(&store));
            let mut engine = acp::PermissionEngine::new(store);
            engine.add_policy(&policy);
            for object in &case.objects {
                acp.register_doc_object(
                    &Did::new(&object.owner)?,
                    &policy.id,
                    &object.resource,
                    &object.id,
                )
                .await?;
            }
            for relation in &case.relations {
                household_relationship(&acp, &policy.id, case, relation, true).await?;
            }
            for enrolled in &case.enrolled {
                Did::new(enrolled)?;
            }
            for (step, action) in case.actions.iter().enumerate() {
                match action {
                    LeanIdentityAction::Grant { relation } => {
                        household_relationship(&acp, &policy.id, case, relation, true).await?;
                    }
                    LeanIdentityAction::Revoke { relation } => {
                        household_relationship(&acp, &policy.id, case, relation, false).await?;
                    }
                    LeanIdentityAction::GrantAs {
                        actor,
                        relation,
                        expected,
                    } => {
                        household_relationship_as(
                            &acp, &policy.id, actor, relation, true, *expected,
                        )
                        .await?;
                    }
                    LeanIdentityAction::RevokeAs {
                        actor,
                        relation,
                        expected,
                    } => {
                        household_relationship_as(
                            &acp, &policy.id, actor, relation, false, *expected,
                        )
                        .await?;
                    }
                    LeanIdentityAction::Check {
                        actor,
                        resource,
                        id,
                        permission,
                        expected,
                    } => {
                        let actor = Did::new(actor)?;
                        let allowed = match permission.as_str() {
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
                        };
                        assert_eq!(
                            allowed, *expected,
                            "{} step {step}: document ACP",
                            case.name
                        );
                    }
                }
            }
        }
        Ok(())
    }
}
