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
    use gents::store::config_client::{
        apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan, NodePermission,
    };
    use gents::store::graphql::{created_doc_id, escape_graphql_string};
    use gents::NodeIdentity;

    let case = lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .expect("modeled configuration case");
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

    let db = crate::support::test_db("modeled-configuration-native").await;
    let fixture = crate::support::managed_agent_fixture(&db).await;
    let node = db.node.clone();
    let node_did = db.node_identity.did();
    let user_access = fixture.access.clone();
    let agent_access = gents::ConfigAccess::local_managed_agent(
        node.clone(),
        Did::new(fixture.agent.did())?,
        fixture.agent.as_ref(),
    )?;
    let sibling_binding = gents::store::identity::provision_agent_identity_authorized(
        &node,
        db.node_identity.as_ref(),
        &Did::new(fixture.user.did())?,
    )
    .await?;
    let sibling_signer = gents::store::identity::resolve_agent_identity(
        db.node_identity.as_ref(),
        &sibling_binding,
    )?;
    let sibling_access = gents::ConfigAccess::local_managed_agent(
        node.clone(),
        Did::new(sibling_signer.did())?,
        sibling_signer.as_ref(),
    )?;
    let runtime_access = gents::ConfigAccess::local_as(node.clone(), Did::new(node_did)?);
    for actual in [
        fixture.user.did(),
        fixture.agent.did(),
        sibling_signer.did(),
    ] {
        for permission in [
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::CollectionGet,
            NodePermission::DacRelationAdd,
            NodePermission::DacRelationDelete,
        ] {
            runtime_access
                .grant_node_permission(&Did::new(actual)?, permission)
                .await?;
        }
    }
    let actual_actor = |modeled: &str| -> anyhow::Result<Did> {
        let actual = if modeled == owner {
            fixture.user.did()
        } else if modeled == agent {
            fixture.agent.did()
        } else if modeled == sibling {
            sibling_signer.did()
        } else if modeled == runtime {
            node_did
        } else {
            anyhow::bail!("unknown modeled actor {modeled}")
        };
        Ok(Did::new(actual)?)
    };
    let access_for = |modeled: &str| -> anyhow::Result<gents::ConfigAccess> {
        if modeled == owner {
            Ok(user_access.clone())
        } else if modeled == agent {
            Ok(agent_access.clone())
        } else if modeled == sibling {
            Ok(sibling_access.clone())
        } else if modeled == runtime {
            Ok(runtime_access.clone())
        } else {
            anyhow::bail!("unknown modeled actor {modeled}")
        }
    };
    assert_eq!(
        case.policy_yaml,
        gents_protocol::schemas::AGENT_OWNERSHIP_POLICY
    );
    let selected =
        serde_json::json!({"node_did":node_did,"tools_id":"selected", "display_name":"initial"});
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: gents::Collection::Tools,
        add: selected.clone(),
        update: selected,
    }])?;
    user_access
        .transact_write("test.configuration.selected", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await })
        })
        .await?;
    let query = |id: &str| {
        format!(
        "{{ Tools(filter: {{node_did: {{_eq: \"{}\"}}, tools_id: {{_eq: \"{}\"}}}}) {{_docID display_name}} }}",
        escape_graphql_string(node_did), escape_graphql_string(id),
    )
    };
    let selected_doc = user_access.execute(&query("selected")).await?["data"]["Tools"][0]["_docID"]
        .as_str()
        .expect("selected Tools physical ID")
        .to_owned();
    let created = agent_access.write("test.configuration.agent_owned", &format!(
        "mutation {{ create_Tools(input: {{node_did: \"{}\", tools_id: \"agent-created\", display_name: \"initial\"}}) {{_docID}} }}",
        escape_graphql_string(node_did),
    )).await?;
    let agent_doc = created_doc_id(&created, "Tools")?;
    let node_doc = user_access
        .transact_readonly("test.configuration.node", |txn| {
            Box::pin(async move {
                Ok::<_, anyhow::Error>(
                    gents::store::config_client::read_desired_state_record_in_txn(
                        txn,
                        gents::Collection::Node,
                        node_did,
                        node_did,
                    )
                    .await?
                    .expect("Node physical ID")
                    .0,
                )
            })
        })
        .await?;
    agent_access
        .add_resource_relationship("Tools", &agent_doc, "parent", "nodes", &node_doc)
        .await?;

    for (step, action) in case.actions.iter().enumerate() {
        match action {
            LeanIdentityAction::Grant { relation } | LeanIdentityAction::Revoke { relation } => {
                let LeanIdentitySubject::Actor { actor } = &relation.subject else {
                    anyhow::bail!("configuration relation must name an actor");
                };
                let actual = actual_actor(actor)?;
                if matches!(action, LeanIdentityAction::Grant { .. }) {
                    user_access
                        .add_actor_relationship("Tools", &selected_doc, &relation.relation, &actual)
                        .await?;
                    if relation.relation == "reader" {
                        assert!(agent_access
                            .add_actor_relationship("Tools", &selected_doc, "writer", &actual)
                            .await
                            .is_err());
                    }
                } else {
                    user_access
                        .delete_actor_relationship(
                            "Tools",
                            &selected_doc,
                            &relation.relation,
                            &actual,
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
                let response = access_for(actor)?.execute(&query(id)).await?;
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
                let access = access_for(actor)?;
                let doc_id = if id == "selected" {
                    &selected_doc
                } else {
                    &agent_doc
                };
                let before = user_access.execute(&query(id)).await?["data"]["Tools"][0]
                    ["display_name"]
                    .clone();
                let value = format!("update-{step}");
                let updated = if actor == owner {
                    let document =
                        serde_json::json!({"node_did":node_did,"tools_id":id,"display_name":value});
                    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                        collection: gents::Collection::Tools,
                        add: document.clone(),
                        update: document,
                    }])?;
                    access
                        .transact_write("test.configuration.update", |txn| {
                            let plan = &plan;
                            Box::pin(async move { apply_desired_state_plan(txn, plan).await })
                        })
                        .await
                        .is_ok()
                } else {
                    match access.write("test.configuration.update", &format!(
                        "mutation {{ update_Tools(docID: \"{}\", input: {{display_name: \"{}\"}}) {{_docID}} }}",
                        escape_graphql_string(doc_id), escape_graphql_string(&value),
                    )).await {
                        Ok(response) => !response["data"]["update_Tools"].as_array().expect("update rows").is_empty(),
                        Err(error) if !*expected && error.is_graphql_rejection() => false,
                        Err(error) => return Err(error.into()),
                    }
                };
                assert_eq!(updated, *expected, "modeled {actor} updating {id}");
                let after = user_access.execute(&query(id)).await?["data"]["Tools"][0]
                    ["display_name"]
                    .clone();
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
    let actual_agent = Did::new(fixture.agent.did())?;
    for observation in &case.configuration_atomic_cases {
        let commit = observation["commit"].as_bool().unwrap();
        let document = serde_json::json!({"node_did":node_did,"tools_id":"atomic"});
        let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
            collection: gents::Collection::Tools,
            add: document.clone(),
            update: document,
        }])?;
        let result = user_access
            .transact_write("test.configuration.atomic", |txn| {
                let plan = &plan;
                let actual_agent = &actual_agent;
                Box::pin(async move {
                    apply_desired_state_plan(txn, plan).await?;
                    let (doc, _) = gents::store::config_client::read_desired_state_record_in_txn(
                        txn,
                        gents::Collection::Tools,
                        node_did,
                        "atomic",
                    )
                    .await?
                    .expect("atomic Tools physical ID");
                    txn.add_actor_relationship("Tools", &doc, "reader", actual_agent)
                        .await?;
                    if !commit {
                        anyhow::bail!("modeled abort");
                    }
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await;
        assert_eq!(
            result.is_ok(),
            commit,
            "modeled transaction outcome: {result:?}"
        );
        for (modeled, field) in [
            (owner, "owner_read"),
            (agent, "agent_read"),
            (sibling, "sibling_read"),
        ] {
            let rows = access_for(modeled)?.execute(&query("atomic")).await?;
            let visible = !rows["data"]["Tools"].as_array().unwrap().is_empty();
            assert_eq!(
                visible,
                observation[field].as_bool().unwrap(),
                "modeled {modeled} {field}"
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
    use anyhow::Context;
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
    let db = crate::support::test_db("modeled-self-config-native").await;
    let node = db.node.clone();
    let runtime = db.node_identity.clone();
    let fixture = crate::support::managed_agent_fixture(&db).await;
    let user = &fixture.user;
    let user_access = fixture.access.clone();
    let agent = &fixture.binding;
    let agent_signer = fixture.agent.clone();
    let sibling = gents::store::identity::provision_agent_identity_authorized(
        &node,
        runtime.as_ref(),
        &Did::new(user.did())?,
    )
    .await?;
    let sibling_signer =
        gents::store::identity::resolve_agent_identity(runtime.as_ref(), &sibling)?;
    let node_admin = gents::ConfigAccess::local_as(node.clone(), Did::new(runtime.did())?);
    for actor in [user.did(), agent_signer.did(), sibling_signer.did()] {
        for permission in [
            gents::store::config_client::NodePermission::DocumentRead,
            gents::store::config_client::NodePermission::DocumentUpdate,
            gents::store::config_client::NodePermission::CollectionGet,
            gents::store::config_client::NodePermission::DacRelationAdd,
            gents::store::config_client::NodePermission::DacRelationDelete,
        ] {
            node_admin
                .grant_node_permission(&Did::new(actor)?, permission)
                .await?;
        }
    }
    let agent_access = gents::ConfigAccess::local_managed_agent(
        node.clone(),
        Did::new(agent_signer.did())?,
        agent_signer.as_ref(),
    )?;
    let sibling_access = gents::ConfigAccess::local_managed_agent(
        node.clone(),
        Did::new(sibling_signer.did())?,
        sibling_signer.as_ref(),
    )?;
    let (agent_doc, agent_value) = user_access
        .transact_readonly("test.self_config.agent", |txn| {
            let runtime = &runtime;
            Box::pin(async move {
                gents::store::config_client::read_desired_state_record_in_txn(
                    txn,
                    gents::Collection::Agent,
                    runtime.did(),
                    &agent.agent_did,
                )
                .await?
                .context("modeled Agent is unavailable")
            })
        })
        .await?;
    let access_for = |did: &str| -> anyhow::Result<gents::ConfigAccess> {
        if did == user.did() {
            Ok(user_access.clone())
        } else if did == agent_signer.did() {
            Ok(agent_access.clone())
        } else if did == sibling_signer.did() {
            Ok(sibling_access.clone())
        } else {
            anyhow::bail!("unknown native household actor {did}")
        }
    };
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
        let access = access_for(caller)?;
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
                let access = access_for(actor(modeled)?.as_str())?;
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
                let access = access_for(actor(modeled)?.as_str())?;
                let response = if actor(modeled)?.as_str() == user.did() {
                    let mut updated = agent_value.clone();
                    updated["description"] = serde_json::json!("configured");
                    let plan = gents::store::config_client::DesiredStateApplyPlan::new(vec![
                        gents::store::config_client::DesiredStateApplyDocument {
                            collection: gents::Collection::Agent,
                            add: agent_value.clone(),
                            update: updated,
                        },
                    ])?;
                    access
                        .transact_write("test.self_config.update", |txn| {
                            let plan = &plan;
                            Box::pin(async move {
                                gents::store::config_client::apply_desired_state_plan(txn, plan)
                                    .await
                            })
                        })
                        .await
                        .map(|_| true)
                } else {
                    access.write("test.self_config.update", &format!(
                        "mutation {{ update_Agent(docID: \"{}\", input: {{description: \"configured\"}}) {{ _docID }} }}",
                        gents_protocol::graphql::escape_graphql_string(&agent_doc),
                    )).await.map(|value| {
                        value.pointer("/data/update_Agent").is_some_and(|rows| {
                            rows.as_array().is_some_and(|rows| !rows.is_empty()) || rows.is_object()
                        })
                    }).map_err(anyhow::Error::from)
                };
                let changed = response.as_ref().is_ok_and(|changed| *changed);
                assert_eq!(changed, *expected, "modeled {action:?}: {response:?}");
            }
            _ => anyhow::bail!("unexpected self-config action {action:?}"),
        }
    }
    node.shutdown().await.expect("node shutdown");
    Ok(())
}
