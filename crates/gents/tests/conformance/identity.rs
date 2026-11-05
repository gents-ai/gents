use crate::lean_vocab_test::{lean_identity_permission_cases, LeanIdentityPermissionCase};
use acp::{
    AcpStore, DocumentACP, DocumentPermission, Identity, LocalDocumentACP, MemoryAcpStore,
    RelationTuple, READER_RELATION,
};
use identity::Did;
use std::sync::Arc;

const IDENTITY_PERMISSION_POLICY_ID: &str = "identity-permission-cases";
const IDENTITY_PERMISSION_RESOURCE_NAME: &str = "row";

async fn build_local_acp_from_lean_case(
    case: &LeanIdentityPermissionCase,
) -> anyhow::Result<LocalDocumentACP> {
    assert!(
        case.permission.ends_with(".read"),
        "case {:?}: only .read permission fixtures are supported by this ACP witness, got {:?}",
        case.name,
        case.permission
    );
    assert!(
        case.permission
            .starts_with(format!("row:{}:", case.row_owner).as_str()),
        "case {:?}: permission {:?} must be scoped to row owner {:?}",
        case.name,
        case.permission,
        case.row_owner
    );

    let store = Arc::new(MemoryAcpStore::new());
    let acp = LocalDocumentACP::new(store.clone());
    let row_owner = did_from_lean_case(&case.row_owner, case, "row_owner");

    acp.register_doc_object(
        &row_owner,
        IDENTITY_PERMISSION_POLICY_ID,
        IDENTITY_PERMISSION_RESOURCE_NAME,
        &case.row_owner,
    )
    .await?;

    let namespaced_resource =
        format!("{IDENTITY_PERMISSION_POLICY_ID}:{IDENTITY_PERMISSION_RESOURCE_NAME}");
    for grant in &case.grants {
        assert_eq!(
            grant.permission, case.permission,
            "case {:?}: grant {:?} targets a different permission than the row under test",
            case.name, grant
        );
        let node = did_from_lean_case(&grant.actor, case, "grant.actor");
        let tuple = RelationTuple::try_new(
            node,
            READER_RELATION,
            namespaced_resource.as_str(),
            case.row_owner.as_str(),
        )?;
        store.put_tuple(&tuple).await?;
    }

    Ok(acp)
}

fn did_from_lean_case(value: &str, case: &LeanIdentityPermissionCase, field: &str) -> Did {
    Did::new(value).unwrap_or_else(|error| {
        panic!(
            "case {:?}: {field} {:?} is not a valid DefraDB identity DID: {error}",
            case.name, value
        )
    })
}

#[tokio::test]
async fn resolved_identity_permission_cases_drive_defra_acp() -> anyhow::Result<()> {
    for case in lean_identity_permission_cases() {
        if case.expected_actor_did.is_none() || case.expected_peer_did.is_none() {
            continue;
        }
        let acp = build_local_acp_from_lean_case(case).await?;
        let actor = Identity::Authenticated(did_from_lean_case(
            case.expected_actor_did.as_deref().expect("resolved actor"),
            case,
            "expected_actor_did",
        ));
        let peer = Identity::Authenticated(did_from_lean_case(
            case.expected_peer_did.as_deref().expect("resolved peer"),
            case,
            "expected_peer_did",
        ));
        let actor_allowed = acp
            .check_doc_access(
                &actor,
                DocumentPermission::Read,
                IDENTITY_PERMISSION_POLICY_ID,
                IDENTITY_PERMISSION_RESOURCE_NAME,
                &case.row_owner,
            )
            .await?;
        let peer_allowed = acp
            .check_doc_access(
                &peer,
                DocumentPermission::Read,
                IDENTITY_PERMISSION_POLICY_ID,
                IDENTITY_PERMISSION_RESOURCE_NAME,
                &case.row_owner,
            )
            .await?;
        assert_eq!(
            actor_allowed, case.expected_actor_allowed,
            "{}: actor ACP permission",
            case.name
        );
        assert_eq!(
            peer_allowed, case.expected_peer_allowed,
            "{}: peer ACP permission",
            case.name
        );
        assert_eq!(
            actor_allowed == peer_allowed,
            case.expected_decisions_equal,
            "{}: ACP decision equality",
            case.name
        );
    }
    Ok(())
}

fn household_subject(
    subject: &crate::lean_vocab_test::LeanIdentitySubject,
) -> anyhow::Result<acp::Subject> {
    use crate::lean_vocab_test::LeanIdentitySubject;
    Ok(match subject {
        LeanIdentitySubject::Actor { actor } => acp::Subject::Entity(Did::new(actor)?),
        LeanIdentitySubject::Object { resource, id } => acp::Subject::entity_set(resource, id, ""),
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
    case: &crate::lean_vocab_test::LeanIdentityHouseholdCase,
    relation: &crate::lean_vocab_test::LeanIdentityRelation,
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
    relation: &crate::lean_vocab_test::LeanIdentityRelation,
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
    use crate::lean_vocab_test::{lean_identity_household_cases, LeanIdentityAction};
    use acp::{
        policy_yaml::{build_policy, parse_policy_yaml},
        MemoryZanzibarStore, ZanzibarDocumentACP, ZanzibarStore,
    };
    for contract in crate::lean_vocab_test::lean_identity_contracts() {
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
            for template in gents::runtime::agent::p2p_reconcile::templates::builtin_templates() {
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
                    household_relationship_as(&acp, &policy.id, actor, relation, true, *expected)
                        .await?;
                }
                LeanIdentityAction::RevokeAs {
                    actor,
                    relation,
                    expected,
                } => {
                    household_relationship_as(&acp, &policy.id, actor, relation, false, *expected)
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

#[tokio::test]
async fn modeled_configuration_grants_use_authenticated_native_relations() -> anyhow::Result<()> {
    use crate::lean_vocab_test::{
        lean_identity_household_cases, LeanIdentityAction, LeanIdentitySubject,
    };
    use gents::store::graphql::{created_doc_id, escape_graphql_string};

    let case = lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .expect("modeled configuration case");
    let node = Arc::new(defra_node::Node::builder().build().await?);
    let owner = &case
        .objects
        .iter()
        .find(|object| object.id == "home")
        .unwrap()
        .owner;
    let agent = &case
        .objects
        .iter()
        .find(|object| object.id == "agent-created")
        .unwrap()
        .owner;
    let owner_access = gents::ConfigAccess::local_as(node.clone(), Did::new(owner)?);
    let agent_access = gents::ConfigAccess::local_as(node.clone(), Did::new(agent)?);
    let context = defra_core::RequestContext::default().with_identity(Some(owner.clone()));
    let policy = node
        .add_dac_policy_with_counter_as(&context, &case.policy_yaml, 0)
        .await?;
    assert_eq!(policy, gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID);
    let policy = escape_graphql_string(&policy);
    owner_access.add_collection(&format!(
        "type Node @policy(id: \"{policy}\", resource: \"nodes\") {{ node_did: String }}\ntype Tools @policy(id: \"{policy}\", resource: \"configuration\") {{ node_did: String, tools_id: String, value: String }}"
    )).await?;
    let node_response = owner_access
        .write(
            "test.configuration.node",
            "mutation { create_Node(input: {node_did: \"test-node\"}) { _docID } }",
        )
        .await?;
    let node_doc = created_doc_id(&node_response, "Node")?;
    let create = |id: &str| {
        format!(
        "mutation {{ create_Tools(input: {{ node_did: \"test-node\", tools_id: \"{}\", value: \"initial\" }}) {{ _docID }} }}",
        escape_graphql_string(id)
    )
    };
    let selected_response = owner_access
        .write("test.configuration.selected", &create("selected"))
        .await?;
    let selected_doc = created_doc_id(&selected_response, "Tools")?;
    let created_response = agent_access
        .write("test.configuration.agent_owned", &create("agent-created"))
        .await?;
    let agent_doc = created_doc_id(&created_response, "Tools")?;
    owner_access
        .add_resource_relationship("Tools", &selected_doc, "parent", "nodes", &node_doc)
        .await?;
    agent_access
        .add_resource_relationship("Tools", &agent_doc, "parent", "nodes", &node_doc)
        .await?;
    let runtime = case
        .relations
        .iter()
        .find_map(|relation| match &relation.subject {
            LeanIdentitySubject::Actor { actor }
                if relation.resource == "nodes" && relation.relation == "runtime_reader" =>
            {
                Some(actor)
            }
            _ => None,
        })
        .expect("modeled runtime reader");
    owner_access
        .add_actor_relationship("Node", &node_doc, "runtime_reader", &Did::new(runtime)?)
        .await?;

    for (step, action) in case.actions.iter().enumerate() {
        match action {
            LeanIdentityAction::Grant { relation } | LeanIdentityAction::Revoke { relation } => {
                let LeanIdentitySubject::Actor { actor } = &relation.subject else {
                    anyhow::bail!("configuration relation must name an actor");
                };
                let actor = Did::new(actor)?;
                if matches!(action, LeanIdentityAction::Grant { .. }) {
                    owner_access
                        .add_actor_relationship("Tools", &selected_doc, &relation.relation, &actor)
                        .await?;
                    if relation.relation == "reader" {
                        assert!(agent_access
                            .add_actor_relationship("Tools", &selected_doc, "writer", &actor)
                            .await
                            .is_err());
                    }
                } else {
                    owner_access
                        .delete_actor_relationship(
                            "Tools",
                            &selected_doc,
                            &relation.relation,
                            &actor,
                        )
                        .await?;
                }
            }
            LeanIdentityAction::Check {
                actor,
                id,
                permission,
                expected,
                ..
            } if permission == "read" => {
                let access = gents::ConfigAccess::local_as(node.clone(), Did::new(actor)?);
                let response = access
                    .execute(&format!(
                        "{{ Tools(filter: {{ tools_id: {{ _eq: \"{}\" }} }}) {{ _docID }} }}",
                        escape_graphql_string(id)
                    ))
                    .await?;
                let visible = !response["data"]["Tools"].as_array().unwrap().is_empty();
                assert_eq!(visible, *expected, "modeled {actor} reading {id}");
            }
            LeanIdentityAction::Check {
                actor,
                id,
                permission,
                expected,
                ..
            } if permission == "update" => {
                let access = gents::ConfigAccess::local_as(node.clone(), Did::new(actor)?);
                let doc_id = if id == "selected" {
                    &selected_doc
                } else {
                    &agent_doc
                };
                let query = format!(
                    "{{ Tools(filter: {{ _docID: {{ _eq: \"{}\" }} }}) {{ value }} }}",
                    escape_graphql_string(doc_id)
                );
                let before =
                    owner_access.execute(&query).await?["data"]["Tools"][0]["value"].clone();
                let value = format!("update-{step}");
                let result = access.write("test.configuration.update", &format!(
                    "mutation {{ update_Tools(docID: \"{}\", input: {{value: \"{}\"}}) {{ _docID }} }}",
                    escape_graphql_string(doc_id), escape_graphql_string(&value)
                )).await;
                let updated = match result {
                    Ok(response) => !response["data"]["update_Tools"]
                        .as_array()
                        .expect("update result rows")
                        .is_empty(),
                    Err(error) if !*expected && error.is_graphql_rejection() => false,
                    Err(error) => return Err(error.into()),
                };
                assert_eq!(updated, *expected, "modeled {actor} updating {id}");
                let after =
                    owner_access.execute(&query).await?["data"]["Tools"][0]["value"].clone();
                assert_eq!(
                    after,
                    if *expected {
                        serde_json::json!(value)
                    } else {
                        before
                    },
                    "modeled {actor} update effect on {id}"
                );
            }
            _ => {}
        }
    }
    let sibling = case
        .actions
        .iter()
        .find_map(|action| match action {
            LeanIdentityAction::Check { actor, .. }
                if actor != owner && actor != agent && actor != runtime =>
            {
                Some(actor)
            }
            _ => None,
        })
        .expect("modeled sibling");
    for observation in &case.configuration_atomic_cases {
        let commit = observation["commit"].as_bool().unwrap();
        let result = owner_access.transact_write("test.configuration.atomic", |txn| {
            let node_doc = &node_doc;
            Box::pin(async move {
                let response = txn.execute("mutation { create_Tools(input: {node_did: \"test-node\", tools_id: \"atomic\"}) { _docID } }").await?;
                let doc_id = created_doc_id(&response, "Tools")?;
                txn.add_resource_relationship("Tools", &doc_id, "parent", "nodes", node_doc).await?;
                txn.add_actor_relationship("Tools", &doc_id, "reader", &Did::new(agent)?).await?;
                if !commit {
                    anyhow::bail!("modeled abort");
                }
                Ok::<_, anyhow::Error>(())
            })
        }).await;
        assert_eq!(
            result.is_ok(),
            commit,
            "modeled transaction outcome: {result:?}"
        );
        for (actor, field) in [
            (owner, "owner_read"),
            (agent, "agent_read"),
            (sibling, "sibling_read"),
        ] {
            let access = gents::ConfigAccess::local_as(node.clone(), Did::new(actor)?);
            let rows = access
                .execute("{ Tools(filter: {tools_id: {_eq: \"atomic\"}}) { _docID } }")
                .await?;
            let visible = !rows["data"]["Tools"].as_array().unwrap().is_empty();
            assert_eq!(
                visible,
                observation[field].as_bool().unwrap(),
                "modeled {actor} {field}"
            );
        }
    }
    node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn modeled_self_config_grants_use_authenticated_native_relations() -> anyhow::Result<()> {
    use crate::lean_vocab_test::{
        lean_identity_household_cases, LeanIdentityAction, LeanIdentitySubject,
    };
    use gents::store::config_client::AgentSelfConfigPermission;
    use gents::NodeIdentity;

    let case = lean_identity_household_cases()
        .first()
        .expect("household case");
    let model_user = &case
        .objects
        .iter()
        .find(|object| object.resource == "nodes" && object.id == "home")
        .expect("modeled Node")
        .owner;
    let executor = |id: &str| -> &str {
        let relation = case
            .relations
            .iter()
            .find(|relation| {
                relation.resource == "agents"
                    && relation.id == id
                    && relation.relation == "executor"
            })
            .expect("modeled executor");
        match &relation.subject {
            LeanIdentitySubject::Actor { actor } => actor,
            LeanIdentitySubject::Object { .. } | LeanIdentitySubject::EntitySet { .. } => {
                panic!("executor is an actor")
            }
        }
    };
    let model_agent = executor("general");
    let model_sibling = executor("sibling");
    let home = tempfile::tempdir()?;
    let runtime = gents::KeyIdentity::load_or_create(home.path().join("runtime.key"), None)?;
    let user = gents::KeyIdentity::load_or_create(home.path().join("user.key"), None)?;
    let agent = gents::store::identity::provision_agent_identity(&runtime)?;
    let sibling = gents::store::identity::provision_agent_identity(&runtime)?;
    let _agent_signer = gents::store::identity::resolve_agent_identity(&runtime, &agent)?;
    let _sibling_signer = gents::store::identity::resolve_agent_identity(&runtime, &sibling)?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/api/v0/graphql", listener.local_addr()?);
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(runtime.did())
            .with_http(defra_node::HttpConfig::with_listener(listener.into_std()?)?)
            .build()
            .await?,
    );
    gents::store::schema::ensure_runtime_schemas(&node).await?;
    let user_access = gents::ConfigAccess::local_as(node.clone(), Did::new(user.did())?);
    let (node_doc, agent_doc) = user_access.transact_write("test.self_config.seed", |txn| {
        let agent = &agent;
        let runtime = &runtime;
        Box::pin(async move {
            let response = txn.execute_with_variables(
                "mutation($input: NodeMutationInputArg!) { create_Node(input: $input) { _docID } }",
                &serde_json::json!({"input": {"node_did": runtime.did()}}),
            ).await?;
            let node_doc = gents::store::graphql::created_doc_id(&response, "Node")?;
            let response = txn.execute_with_variables(
                "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID } }",
                &serde_json::json!({"input": {"node_did": runtime.did(),
                    "agent_did": agent.agent_did, "creation_nonce": agent.creation_nonce}}),
            ).await?;
            let agent_doc = gents::store::graphql::created_doc_id(&response, "Agent")?;
            Ok::<_, anyhow::Error>((node_doc, agent_doc))
        })
    }).await?;
    user_access
        .add_resource_relationship("Agent", &agent_doc, "parent", "nodes", &node_doc)
        .await?;
    user_access
        .add_actor_relationship(
            "Agent",
            &agent_doc,
            "executor",
            &Did::new(&agent.agent_did)?,
        )
        .await?;
    let actor = |modeled: &str| -> anyhow::Result<Did> {
        let did = if modeled == model_user {
            user.did()
        } else if modeled == model_agent {
            &agent.agent_did
        } else if modeled == model_sibling {
            &sibling.agent_did
        } else {
            anyhow::bail!("unknown modeled actor {modeled}")
        };
        Ok(Did::new(did)?)
    };
    for observation in &case.self_config_lookup_cases {
        let (caller, requested): (&str, &str) = match (
            observation.present,
            observation.visible,
            observation.same_generation,
            observation.native_allows,
        ) {
            (false, false, _, _) => (user.did(), &sibling.agent_did),
            (true, false, _, _) => (&sibling.agent_did, &agent.agent_did),
            (true, true, true, false) => (&agent.agent_did, &agent.agent_did),
            (true, true, true, true) => (user.did(), &agent.agent_did),
            // ACP1 cannot fence a document replacement between lookup and grant.
            (true, true, false, _) => continue,
            _ => anyhow::bail!("unrepresentable modeled lookup case {}", observation.name),
        };
        let access = gents::ConfigAccess::Graphql(
            gents::store::config_client::GraphqlEndpoint::as_principal(endpoint.clone(), caller),
        );
        let outcome = match access
            .set_agent_self_config_permission(
                runtime.did(),
                requested,
                AgentSelfConfigPermission::Grant,
            )
            .await
        {
            Ok(()) => "applied",
            Err(gents::store::config_client::AgentSelfConfigPermissionError::AgentUnavailable) => {
                "unavailable"
            }
            Err(gents::store::config_client::AgentSelfConfigPermissionError::Access(
                gents::store::config_client::ConfigClientError::DocumentRelationshipStatus {
                    status,
                    ..
                },
            )) if status.is_client_error() => "denied",
            Err(error) => return Err(error.into()),
        };
        assert_eq!(
            outcome, observation.expected,
            "modeled {}",
            observation.name
        );
        if outcome == "applied" {
            access
                .set_agent_self_config_permission(
                    runtime.did(),
                    requested,
                    AgentSelfConfigPermission::Revoke,
                )
                .await?;
        }
    }
    for action in &case.self_config_actions {
        match action {
            LeanIdentityAction::GrantAs {
                actor: modeled,
                expected,
                ..
            }
            | LeanIdentityAction::RevokeAs {
                actor: modeled,
                expected,
                ..
            } => {
                let access = gents::ConfigAccess::Graphql(
                    gents::store::config_client::GraphqlEndpoint::as_principal(
                        endpoint.clone(),
                        actor(modeled)?.to_string(),
                    ),
                );
                let permission = if matches!(action, LeanIdentityAction::GrantAs { .. }) {
                    AgentSelfConfigPermission::Grant
                } else {
                    AgentSelfConfigPermission::Revoke
                };
                let result = access
                    .set_agent_self_config_permission(runtime.did(), &agent.agent_did, permission)
                    .await;
                assert_eq!(result.is_ok(), *expected, "modeled {action:?}: {result:?}");
            }
            LeanIdentityAction::Check {
                actor: modeled,
                expected,
                ..
            } => {
                let allowed = node
                    .document_acp()
                    .check_doc_access(
                        &Identity::Authenticated(actor(modeled)?),
                        DocumentPermission::Update,
                        gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID,
                        "agents",
                        &agent_doc,
                    )
                    .await?;
                assert_eq!(allowed, *expected, "modeled {action:?}");
                let access = gents::ConfigAccess::Graphql(
                    gents::store::config_client::GraphqlEndpoint::as_principal(
                        endpoint.clone(),
                        actor(modeled)?.to_string(),
                    ),
                );
                let response = access.write("test.self_config.update", &format!(
                    "mutation {{ update_Agent(docID: \"{}\", input: {{description: \"configured\"}}) {{ _docID }} }}",
                    gents_protocol::graphql::escape_graphql_string(&agent_doc),
                )).await;
                let changed = response
                    .as_ref()
                    .ok()
                    .and_then(|value| value.pointer("/data/update_Agent"))
                    .is_some_and(|rows| {
                        rows.as_array().is_some_and(|rows| !rows.is_empty()) || rows.is_object()
                    });
                assert_eq!(changed, *expected, "modeled {action:?}: {response:?}");
            }
            _ => anyhow::bail!("unexpected self-config action {action:?}"),
        }
    }
    node.shutdown().await.expect("node shutdown");
    Ok(())
}
