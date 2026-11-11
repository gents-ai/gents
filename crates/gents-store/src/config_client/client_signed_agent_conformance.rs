use super::client_signed_agent::{create_route, CreateRoute};

#[test]
fn signed_update_uses_one_physical_target_with_additional_scope_predicates() {
    let variables = serde_json::json!({"input": {"archived_at": null}});
    let scoped = r#"mutation($input: AgentSessionMutationInputArg!) {
        update_AgentSession(filter: {_docID: {_eq: "physical"}, node_did: {_eq: "node"}, session_id: {_eq: "session"}}, input: $input) {_docID}
    }"#;
    assert_eq!(
        super::client_signed_agent::update_target(scoped, &variables, "AgentSession").unwrap(),
        "physical"
    );
    let both = r#"mutation($input: AgentSessionMutationInputArg!) {
        update_AgentSession(docID: "physical", filter: {node_did: {_eq: "node"}}, input: $input) {_docID}
    }"#;
    assert!(super::client_signed_agent::update_target(both, &variables, "AgentSession").is_err());
    assert!(super::client_signed_agent::update_target(scoped, &variables, "AgentMessage").is_err());
}

#[test]
fn client_goal_graphs_use_the_bundled_canonical_lineage() {
    for (name, sdl) in [
        (
            gents_protocol::schemas::AGENT_REQUEST_NAME,
            gents_protocol::schemas::AGENT_REQUEST,
        ),
        (
            gents_protocol::schemas::AGENT_SESSION_GOAL_CONTROL_NAME,
            gents_protocol::schemas::AGENT_SESSION_GOAL_CONTROL,
        ),
    ] {
        let schema = super::client_signed_agent::signing_schema(sdl, name).unwrap();
        let bundled = query::parse_sdl(sdl).unwrap().remove(0);
        assert_eq!(schema.collection_id, bundled.collection_id);
        assert_eq!(schema.version_id, bundled.version_id);
    }
}

#[tokio::test]
async fn publication_observer_refuses_local_transactions_before_callback() {
    use super::{
        AgentPublicationObserver, ConfigAccess, ConfigClientError, ConfigRelationshipGrant,
    };
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    struct Never;
    #[async_trait::async_trait]
    impl AgentPublicationObserver for Never {
        async fn before_commit(
            &self,
            _: &str,
            _: &[ConfigRelationshipGrant],
        ) -> Result<(), ConfigClientError> {
            panic!("observer must not run")
        }
        async fn committed(&self) -> Result<(), ConfigClientError> {
            panic!("observer must not run")
        }
        async fn commit_failed(&self, _: bool) -> Result<(), ConfigClientError> {
            panic!("observer must not run")
        }
        async fn before_grant(&self, _: &ConfigRelationshipGrant) -> Result<(), ConfigClientError> {
            panic!("observer must not run")
        }
        async fn granted(&self, _: &ConfigRelationshipGrant) -> Result<(), ConfigClientError> {
            panic!("observer must not run")
        }
    }
    let node = Arc::new(defra_node::Node::builder().build().await.unwrap());
    let access = ConfigAccess::local(node);
    let invoked = AtomicBool::new(false);
    let result = access
        .transact_write_with_agent_publication("test.local_publication", Arc::new(Never), |_| {
            invoked.store(true, Ordering::SeqCst);
            Box::pin(async { Ok::<_, ConfigClientError>(()) })
        })
        .await;
    assert!(matches!(
        result,
        Err(ConfigClientError::ClientAgentTransportRequired)
    ));
    assert!(!invoked.load(Ordering::SeqCst));
}

#[test]
fn modeled_publication_journal_traces_drive_the_native_owner() {
    use super::client_agent_publication::{ClientAgentJournal, PublicationStage};
    use gents_lean_vocab::{
        LeanIdentityClientAgentJournalAction as Action,
        LeanIdentityClientAgentJournalStage as Stage,
    };

    fn stage(value: &Stage) -> PublicationStage {
        match value {
            Stage::Commit => PublicationStage::Commit,
            Stage::Parent => PublicationStage::Parent,
            Stage::Executor => PublicationStage::Executor,
        }
    }
    fn check(
        actual: &ClientAgentJournal,
        expected: &gents_lean_vocab::LeanIdentityClientAgentJournalState,
        name: &str,
    ) {
        assert_eq!(actual.client, expected.client, "{name}");
        assert_eq!(actual.agent_did, expected.agent_did, "{name}");
        assert_eq!(actual.node_did, expected.node_did, "{name}");
        assert_eq!(actual.agent_doc, expected.agent_doc, "{name}");
        assert_eq!(actual.node_doc, expected.node_doc, "{name}");
        assert_eq!(
            format!("{:?}", actual.commit),
            format!("{:?}", expected.commit),
            "{name}"
        );
        assert_eq!(
            format!("{:?}", actual.parent),
            format!("{:?}", expected.parent),
            "{name}"
        );
        assert_eq!(
            format!("{:?}", actual.executor),
            format!("{:?}", expected.executor),
            "{name}"
        );
        assert_eq!(
            format!("{:?}", actual.permit),
            format!("{:?}", expected.permit),
            "{name}"
        );
    }
    for case in &gents_lean_vocab::lean_contract_snapshot().identity_client_agent_journal_cases {
        let initial = &case.initial;
        let mut journal = ClientAgentJournal::new(
            initial.client.clone(),
            initial.agent_did.clone(),
            initial.node_did.clone(),
            initial.agent_doc.clone(),
            initial.node_doc.clone(),
        );
        check(&journal, initial, &case.name);
        for step in &case.steps {
            check(&journal, &step.before, &case.name);
            match &step.action {
                Action::Record {
                    stage: value,
                    durable,
                } => journal.record_attempt(stage(value), *durable),
                Action::Dispatch { stage: value } => journal.dispatch(stage(value)),
                Action::Finish {
                    stage: value,
                    success,
                    confirmed_failure,
                } => journal.finish_attempt(stage(value), *success, *confirmed_failure),
                Action::Restart => journal.restart(),
                Action::SaveUnrelated => {}
            }
            check(&journal, &step.after, &case.name);
        }
    }
}

#[test]
fn modeled_agent_create_intent_drives_the_transaction_route() {
    let cases = &gents_lean_vocab::lean_contract_snapshot().identity_client_agent_creation_cases;
    let mut covered = 0;
    for case in cases {
        if !case.signer_available
            || !case.supported
            || !case.signer_matches_bearer
            || !case.input_matches_graph
            || !case.trusted_schema
            || !case.graph_verified
            || !case.transport_ready
        {
            continue;
        }
        let actual = create_route(
            case.client_intent,
            case.agent_collection,
            case.create,
            case.http,
        );
        let expected = match case.expected_route.as_str() {
            "native" => CreateRoute::Native,
            "signed" => CreateRoute::Signed,
            "refuse" => CreateRoute::Refuse,
            value => panic!("unknown modeled route {value}"),
        };
        assert_eq!(actual, expected, "{}", case.name);
        covered += 1;
    }
    assert!(covered > 0);
}

#[test]
fn modeled_authored_document_creation_requires_trusted_schema_and_held_user() {
    use super::authored_document::{document_create_allowed, DocumentSchemaSource};
    for case in &gents_lean_vocab::lean_contract_snapshot().identity_client_document_creation_cases
    {
        let flag = |name: &str| case[name].as_bool().expect("modeled boolean");
        let source = match case["source"].as_str().expect("modeled schema source") {
            "authored" => DocumentSchemaSource::UserAuthored,
            "remote" => DocumentSchemaSource::RemoteObservation,
            other => panic!("unknown schema source {other}"),
        };
        let actual = document_create_allowed(
            source,
            flag("single_collection"),
            flag("application_collection"),
            flag("matches_live"),
            flag("held_signer"),
            flag("http"),
        );
        assert_eq!(
            actual,
            case["expected_route"] == "signed",
            "{}",
            case["name"]
        );
    }
}

#[test]
fn modeled_configuration_create_collections_use_trusted_schemas() {
    let case = gents_lean_vocab::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .expect("configuration contract");
    for modeled in &case.configuration_signed_create_collections {
        let Some(collection) = crate::collection::Collection::ALL
            .into_iter()
            .find(|collection| collection.graphql_type() == modeled.collection)
        else {
            assert!(!modeled.supported, "{}", modeled.collection);
            continue;
        };
        let actual = super::client_signed_agent::trusted_create_schema(collection);
        assert_eq!(
            actual.is_some(),
            modeled.supported,
            "{}",
            modeled.collection
        );
        if modeled.supported {
            let route = super::client_signed_agent::update_route(true, collection, true);
            assert_eq!(route == CreateRoute::Signed, modeled.update_supported);
            assert_ne!(route, CreateRoute::Native);
        }
        if let Some(schema) = actual {
            let definitions = query::parse_sdl(schema).expect("trusted schema parses");
            assert_eq!(definitions.len(), 1);
            assert_eq!(definitions[0].name, modeled.collection);
        }
    }
}

#[test]
fn modeled_client_intent_refuses_ordinary_mutations() {
    let case = gents_lean_vocab::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap();
    for observation in &case.ordinary_client_mutation_cases {
        assert_eq!(
            super::client_signed_agent::ordinary_mutation_allowed(
                observation.intent,
                observation.mutation
            ),
            observation.allowed
        );
    }
}

#[test]
fn modeled_configuration_delete_collections_use_trusted_schemas() {
    let case = gents_lean_vocab::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap();
    for modeled in &case.configuration_signed_delete_collections {
        let name = modeled["collection"].as_str().unwrap();
        let schema = super::client_signed_agent::trusted_delete_schema(name);
        assert_eq!(
            schema.is_some(),
            modeled["supported"].as_bool().unwrap(),
            "{name}"
        );
        if let Some(schema) = schema {
            let parsed = query::parse_sdl(schema).unwrap();
            assert_eq!(parsed.len(), 1);
            assert_eq!(parsed[0].name, name);
            match name {
                "Goal" => assert_eq!(schema, gents_protocol::schemas::GOAL),
                "GoalCreationClaim" => {
                    assert_eq!(schema, gents_protocol::schemas::GOAL_CREATION_CLAIM)
                }
                _ => {}
            }
            assert!(super::client_signed_agent::signing_schema(schema, name).is_ok());
        }
    }
}

#[test]
fn modeled_signed_write_replay_requires_confirmed_abort_and_authority() {
    let case = gents_lean_vocab::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap();
    for observation in &case.configuration_signed_write_retry_cases {
        assert_eq!(
            super::txn::client_signed_write_may_retry(
                observation.dispatched,
                observation.confirmed_abort,
                observation.replay_authorized,
            ),
            observation.may_retry,
        );
    }
}

#[test]
fn modeled_agent_immutable_replacements_preserve_genesis() {
    let case = gents_lean_vocab::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap();
    for observation in &case.configuration_immutable_cases {
        for field in ["agent_did", "creation_nonce", "node_did"] {
            let current = serde_json::json!({(field): observation.observed});
            let proposed =
                serde_json::json!({(field): observation.proposed, "display_name": "new"});
            let patch = super::client_signed_agent::configuration_replacement_patch(
                crate::collection::Collection::Agent,
                &current,
                &proposed,
            );
            assert_eq!(patch.is_ok(), observation.allowed, "{field}");
            if let Ok(patch) = patch {
                assert!(patch.get(field).is_none());
                assert_eq!(patch["display_name"], "new");
            }
        }
    }
}

#[test]
fn modeled_held_signer_binds_the_actor_and_bearer() -> anyhow::Result<()> {
    use crate::identity::NodeIdentity;
    let directory = tempfile::tempdir()?;
    let path_a = directory.path().join("a.key");
    let path_b = directory.path().join("b.key");
    let actor_a = crate::identity::KeyIdentity::load_or_create(&path_a, None)?;
    let actor_b = crate::identity::KeyIdentity::load_or_create(&path_b, None)?;
    let key_a = crate::identity::load_file_identity(&path_a)?.private_key_bytes();
    let key_b = crate::identity::load_file_identity(&path_b)?.private_key_bytes();
    let capability_a = super::ClientSigningCapability::from_ed25519_key(&key_a)?;
    let capability_b = super::ClientSigningCapability::from_ed25519_key(&key_b)?;
    let did = |label: &str| {
        if label == "user-a" {
            actor_a.did()
        } else {
            actor_b.did()
        }
    };
    let case = gents_lean_vocab::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap();
    for observation in &case.held_client_signer_cases {
        let actor = did(&observation.actor);
        let endpoint = if observation.bearer == observation.actor {
            super::GraphqlEndpoint::as_principal("http://127.0.0.1:1/api/v0/graphql", actor)
        } else {
            super::GraphqlEndpoint::with_authenticated_delegation(
                "http://127.0.0.1:1/api/v0/graphql",
                "Bearer forged",
                identity::Did::new(actor)?,
            )?
        };
        let result = if observation.has_private {
            let capability = if observation.signer == "user-a" {
                capability_a.clone()
            } else {
                capability_b.clone()
            };
            endpoint
                .with_client_signing_capability(capability)
                .and_then(|endpoint| {
                    super::client_signed_agent::client_signer(&endpoint).map(|_| ())
                })
        } else {
            super::client_signed_agent::client_signer(&endpoint).map(|_| ())
        };
        assert_eq!(result.is_ok(), observation.allowed, "{observation:?}");
    }
    Ok(())
}

#[tokio::test]
async fn modeled_missing_client_signer_fails_before_connecting() {
    let case = gents_lean_vocab::lean_contract_snapshot()
        .identity_client_agent_creation_cases
        .iter()
        .find(|case| case.name == "no-signer")
        .expect("modeled missing-signer case");
    let endpoint = super::GraphqlEndpoint::as_principal(
        "http://127.0.0.1:1/api/v0/graphql",
        "did:key:missing-client-signer",
    )
    .with_client_signed_agent_creates();
    let error = super::client_signed_agent::execute(
        &endpoint,
        "1",
        "mutation { create_Agent(input: {}) { _docID } }",
        &serde_json::json!({"input": {}}),
        gents_protocol::schemas::AGENT.to_owned(),
        "Agent".to_owned(),
    )
    .await
    .expect_err("a missing signer cannot fall back to the runtime");
    assert_eq!(case.expected_route, "refuse");
    assert!(matches!(
        error,
        super::ConfigClientError::ClientAgentSignerRequired
    ));
}

#[tokio::test]
async fn client_agent_genesis_is_signed_by_the_authenticated_desktop_principal(
) -> anyhow::Result<()> {
    use crate::identity::NodeIdentity;
    use serde_json::json;
    use std::sync::Arc;

    let directory = tempfile::tempdir()?;
    let actor =
        crate::identity::KeyIdentity::load_or_create(directory.path().join("desktop.key"), None)?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let node = Arc::new(
        defra_node::Node::builder()
            .enable_signing()
            .with_node_acp_owner(actor.did().to_owned())
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .build()
            .await?,
    );
    let context = defra_core::RequestContext::default().with_identity(Some(actor.did().to_owned()));
    let policy = node
        .add_dac_policy_with_counter_as(
            &context,
            gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
            0,
        )
        .await?;
    assert_eq!(policy, gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID);
    node.add_collection(gents_protocol::schemas::AGENT).await?;
    let endpoint = super::GraphqlEndpoint::as_principal(
        format!("http://{address}/api/v0/graphql"),
        actor.did(),
    )
    .with_client_signing_capability(actor.client_signing_capability()?)?
    .with_client_signed_agent_creates();
    let access = super::ConfigAccess::Graphql(endpoint);
    let input = json!({
        "agent_did": actor.did(), "creation_nonce": "desktop-test",
        "node_did": node.node_identity_did().unwrap(), "display_name": "Client agent",
        "description": null, "context_id": null, "inference_profile_id": "profile",
        "enabled": true, "tags": ["one", "two"], "created_at": null,
        "updated_at": "2026-08-14T12:00:00.120000000Z"
    });
    let response = access
        .transact_write("test.client_signed_agent", |txn| {
            let input = &input;
            Box::pin(async move {
                txn.execute_desired_state_mutation(
                    "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID _version { cid } } }",
                    &json!({"input": input}),
                    crate::collection::Collection::Agent,
                    true,
                )
                .await
            })
        })
        .await?;
    let rows = &response["data"]["add_Agent"];
    let cid = rows[0]["_version"][0]["cid"].as_str().expect("genesis CID");
    let (block, signature) = node
        .authorized_signed_block_bytes(cid, Some(actor.did()))
        .await?;
    let block = defra_core::block::Block::from_dag_cbor(&block)?;
    let signature = defra_core::block::Signature::from_dag_cbor(&signature)?;
    let signer = db::block::verify::verified_signature_signer_did(&block, &signature)
        .map_err(anyhow::Error::msg)?;
    assert_eq!(signer, actor.did());
    assert_ne!(node.node_identity_did(), Some(actor.did()));
    assert!(rows[0]["_docID"].is_string());
    node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn large_client_signed_agent_document_crosses_websocket_frame_boundaries(
) -> anyhow::Result<()> {
    use crate::identity::NodeIdentity;
    use serde_json::json;
    use std::sync::Arc;

    let directory = tempfile::tempdir()?;
    let actor =
        crate::identity::KeyIdentity::load_or_create(directory.path().join("desktop.key"), None)?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let node = Arc::new(
        defra_node::Node::builder()
            .enable_signing()
            .with_node_acp_owner(actor.did().to_owned())
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .build()
            .await?,
    );
    let context = defra_core::RequestContext::default().with_identity(Some(actor.did().to_owned()));
    let policy = node
        .add_dac_policy_with_counter_as(
            &context,
            gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
            0,
        )
        .await?;
    assert_eq!(policy, gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID);
    node.add_collection(gents_protocol::schemas::AGENT).await?;
    let endpoint = super::GraphqlEndpoint::as_principal(
        format!("http://{address}/api/v0/graphql"),
        actor.did(),
    )
    .with_client_signing_capability(actor.client_signing_capability()?)?
    .with_client_signed_agent_creates();
    let access = super::ConfigAccess::Graphql(endpoint);
    let input = json!({
        "agent_did": actor.did(), "creation_nonce": "large-desktop-test",
        "node_did": node.node_identity_did().unwrap(), "display_name": "Client agent",
        "description": "history ✓".repeat(200_000), "context_id": null, "inference_profile_id": "profile",
        "enabled": true, "tags": ["one", "two"], "created_at": null,
        "updated_at": "2026-08-14T12:00:00.120000000Z"
    });
    let response = access
        .transact_write("test.large_client_signed_agent", |txn| {
            let input = &input;
            Box::pin(async move {
                txn.execute_desired_state_mutation(
                    "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID _version { cid } } }",
                    &json!({"input": input}),
                    crate::collection::Collection::Agent,
                    true,
                )
                .await
            })
        })
        .await?;
    let rows = &response["data"]["add_Agent"];
    let cid = rows[0]["_version"][0]["cid"].as_str().expect("genesis CID");
    let (block, signature) = node
        .authorized_signed_block_bytes(cid, Some(actor.did()))
        .await?;
    let block = defra_core::block::Block::from_dag_cbor(&block)?;
    let signature = defra_core::block::Signature::from_dag_cbor(&signature)?;
    let signer = db::block::verify::verified_signature_signer_did(&block, &signature)
        .map_err(anyhow::Error::msg)?;
    assert_eq!(signer, actor.did());
    assert_ne!(node.node_identity_did(), Some(actor.did()));
    assert!(rows[0]["_docID"].is_string());
    node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn client_configuration_creates_are_signed_by_the_authenticated_principal(
) -> anyhow::Result<()> {
    use crate::identity::NodeIdentity;
    use serde_json::json;
    use std::sync::Arc;

    let directory = tempfile::tempdir()?;
    let actor =
        crate::identity::KeyIdentity::load_or_create(directory.path().join("user.key"), None)?;
    let editor =
        crate::identity::KeyIdentity::load_or_create(directory.path().join("agent.key"), None)?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let node = Arc::new(
        defra_node::Node::builder()
            .enable_signing()
            .with_node_acp_owner(actor.did().to_owned())
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .build()
            .await?,
    );
    let context = defra_core::RequestContext::default().with_identity(Some(actor.did().to_owned()));
    node.add_dac_policy_with_counter_as(
        &context,
        gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
        0,
    )
    .await?;
    for permission in [
        acp::nac::NodePermission::DocumentRead,
        acp::nac::NodePermission::DocumentUpdate,
        acp::nac::NodePermission::DocumentDelete,
        acp::nac::NodePermission::CollectionGet,
    ] {
        node.add_nac_actor_relationship_as(&context, permission.as_str(), editor.did())
            .await?;
    }
    node.add_collection(gents_protocol::schemas::AGENT).await?;
    node.add_collection(gents_protocol::schemas::AGENT_CONTEXT)
        .await?;
    node.add_collection(gents_protocol::schemas::TOOLS).await?;
    node.add_collection(gents_protocol::schemas::TASK).await?;
    node.add_collection(gents_protocol::schemas::SCHEDULE)
        .await?;
    node.add_collection(gents_protocol::schemas::TRIGGER)
        .await?;
    node.add_collection(gents_protocol::schemas::INFERENCE_BACKEND_METADATA)
        .await?;
    let backend_schema = node.get_collection("InferenceBackendMetadata")?.unwrap();
    let approved_schema = super::client_signed_agent::signing_schema(
        gents_protocol::schemas::INFERENCE_BACKEND_METADATA,
        "InferenceBackendMetadata",
    )?;
    assert_eq!(backend_schema.version_id, approved_schema.version_id);
    assert_eq!(backend_schema.collection_id, approved_schema.collection_id);
    node.add_collection(gents_protocol::schemas::CALLBACK)
        .await?;
    node.add_collection(gents_protocol::schemas::CALLBACK_BINDING)
        .await?;
    node.add_collection(gents_protocol::schemas::CALLBACK_MODULE)
        .await?;
    let url = format!("http://{address}/api/v0/graphql");
    let spoofed = super::GraphqlEndpoint::as_principal(&url, actor.did())
        .with_client_signing_capability(editor.client_signing_capability()?);
    assert!(matches!(
        spoofed,
        Err(super::ConfigClientError::ClientSignerMismatch)
    ));
    let did_only = super::GraphqlEndpoint::as_principal(&url, actor.did());
    assert!(matches!(
        did_only.authorization(),
        Err(super::ConfigClientError::ClientAgentSignerRequired)
    ));
    let did_only = super::ConfigAccess::Graphql(did_only);
    assert!(did_only
        .execute("{ AgentContext { _docID } }")
        .await
        .is_err());
    let access = super::ConfigAccess::Graphql(
        super::GraphqlEndpoint::as_principal(&url, actor.did())
            .with_client_signing_capability(actor.client_signing_capability()?)?
            .with_client_signed_agent_creates(),
    );
    let blocked = access
        .write(
            "test.unsigned_client_write",
            "mutation { create_Task(input: {task_id: \"wrong\"}) { _docID } }",
        )
        .await;
    assert!(matches!(
        blocked,
        Err(super::ConfigClientError::ClientUnsignedMutationUnsupported)
    ));
    let blocked = access
        .transact_write("test.unsigned_client_delete", |txn| {
            Box::pin(async move {
                txn.execute("mutation { delete_Task(filter: {_docID: {_eq: \"wrong\"}}) {_docID} }")
                    .await
            })
        })
        .await;
    assert!(matches!(
        blocked,
        Err(super::ConfigClientError::ClientUnsignedMutationUnsupported)
    ));
    for (collection, mutation, input, update, patch) in [
        (crate::collection::Collection::Agent,
         "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID _version { cid } } }",
         json!({"agent_did": "did:key:agent", "creation_nonce": "nonce", "node_did": "did:key:node", "display_name": "before"}),
         "mutation($id: ID!, $input: AgentMutationInputArg!) { update_Agent(docID: $id, input: $input) { _docID _version { cid } } }",
         json!({"display_name": "updated"})),
        (crate::collection::Collection::AgentContext,
         "mutation($input: AgentContextMutationInputArg!) { create_AgentContext(input: $input) { _docID _version { cid } } }",
         json!({"node_did": "did:key:node", "context_id": "context", "system_prompt": "test"}),
         "mutation($id: ID!, $input: AgentContextMutationInputArg!) { update_AgentContext(docID: $id, input: $input) { _docID _version { cid } } }",
         json!({"system_prompt": "updated"})),
        (crate::collection::Collection::Task,
         "mutation($input: TaskMutationInputArg!) { create_Task(input: $input) { _docID _version { cid } } }",
         json!({"node_did": "did:key:node", "task_id": "task", "display_name": "before"}),
         "mutation($id: ID!, $input: TaskMutationInputArg!) { update_Task(docID: $id, input: $input) { _docID _version { cid } } }",
         json!({"display_name": "updated"})),
        (crate::collection::Collection::Schedule,
         "mutation($input: ScheduleMutationInputArg!) { create_Schedule(input: $input) { _docID _version { cid } } }",
         json!({"node_did": "did:key:node", "schedule_id": "schedule", "display_name": "before"}),
         "mutation($id: ID!, $input: ScheduleMutationInputArg!) { update_Schedule(docID: $id, input: $input) { _docID _version { cid } } }",
         json!({"display_name": "updated"})),
        (crate::collection::Collection::Trigger,
         "mutation($input: TriggerMutationInputArg!) { create_Trigger(input: $input) { _docID _version { cid } } }",
         json!({"node_did": "did:key:node", "trigger_id": "trigger", "display_name": "before"}),
         "mutation($id: ID!, $input: TriggerMutationInputArg!) { update_Trigger(docID: $id, input: $input) { _docID _version { cid } } }",
         json!({"display_name": "updated"})),
        (crate::collection::Collection::InferenceBackend,
         "mutation($input: InferenceBackendMetadataMutationInputArg!) { create_InferenceBackendMetadata(input: $input) { _docID _version { cid } } }",
         json!({"node_did": "did:key:node", "backend_id": "backend", "name": "Local", "provider_kind": "OpenAiCompatible", "endpoint": "http://127.0.0.1:8000/v1", "auth_kind": "unauthenticated", "max_concurrent": 2, "enabled": true, "updated_at": "2026-11-05T18:02:46Z"}),
         "mutation($id: ID!, $input: InferenceBackendMetadataMutationInputArg!) { update_InferenceBackendMetadata(docID: $id, input: $input) { _docID _version { cid } } }",
         json!({"name": "updated"})),
        (crate::collection::Collection::Tools,
         "mutation($input: ToolsMutationInputArg!) { create_Tools(input: $input) { _docID _version { cid } } }",
         json!({"node_did": "did:key:node", "tools_id": "tools"}),
         "mutation($id: ID!, $input: ToolsMutationInputArg!) { update_Tools(docID: $id, input: $input) { _docID _version { cid } } }",
         json!({"host": {"enabled": true}})),
        (crate::collection::Collection::Callback,
         "mutation($input: CallbackMutationInputArg!) { create_Callback(input: $input) { _docID _version { cid } } }",
         json!({"node_did": "did:key:node", "callback_id": "callback", "handler": {"kind":"built_in", "emitter":"create_workspace"}}),
         "mutation($id: ID!, $input: CallbackMutationInputArg!) { update_Callback(docID: $id, input: $input) { _docID _version { cid } } }",
         json!({"display_name": "updated"})),
        (crate::collection::Collection::CallbackBinding,
         "mutation($input: CallbackBindingMutationInputArg!) { create_CallbackBinding(input: $input) { _docID _version { cid } } }",
         json!({"node_did": "did:key:node", "binding_id": "binding", "agent_did": editor.did(), "callback_id": "callback", "event_source_id": "source"}),
         "mutation($id: ID!, $input: CallbackBindingMutationInputArg!) { update_CallbackBinding(docID: $id, input: $input) { _docID _version { cid } } }",
         json!({"enabled": false})),
        (crate::collection::Collection::CallbackModule,
         "mutation($input: CallbackModuleMutationInputArg!) { create_CallbackModule(input: $input) { _docID _version { cid } } }",
         json!({"node_did": "did:key:node", "module_id": "module", "artifact_bytes": ""}),
         "mutation($id: ID!, $input: CallbackModuleMutationInputArg!) { update_CallbackModule(docID: $id, input: $input) { _docID _version { cid } } }",
         json!({"enabled": false})),
    ] {
        let response = access.transact_write("test.client_signed_configuration", |txn| {
            let input = &input;
            Box::pin(async move {
                txn.execute_desired_state_mutation(mutation, &json!({"input": input}), collection, true).await
            })
        }).await?;
        let rows = &response["data"][format!("add_{}", collection.graphql_type())];
        let cid = rows[0]["_version"][0]["cid"].as_str().expect("genesis CID");
        let (block, signature) = node.authorized_signed_block_bytes(cid, Some(actor.did())).await?;
        let block = defra_core::block::Block::from_dag_cbor(&block)?;
        let signature = defra_core::block::Signature::from_dag_cbor(&signature)?;
        let signer = db::block::verify::verified_signature_signer_did(&block, &signature)
            .map_err(anyhow::Error::msg)?;
        assert_eq!(signer, actor.did());
        let doc_id = rows[0]["_docID"].as_str().unwrap().to_owned();
        let response = access.transact_write("test.client_configuration_update", |txn| {
            let doc_id = &doc_id;
            let patch = &patch;
            Box::pin(async move {
                txn.execute_desired_state_mutation(update, &json!({"id":doc_id, "input":patch}), collection, false).await
            })
        }).await?;
        let rows = &response["data"][format!("update_{}", collection.graphql_type())];
        let cid = rows[0]["_version"][0]["cid"].as_str().expect("update CID");
        let (block, signature) = node.authorized_signed_block_bytes(cid, Some(actor.did())).await?;
        let block = defra_core::block::Block::from_dag_cbor(&block)?;
        let signature = defra_core::block::Signature::from_dag_cbor(&signature)?;
        let signer = db::block::verify::verified_signature_signer_did(&block, &signature)
            .map_err(anyhow::Error::msg)?;
        assert_eq!(signer, actor.did());
        if collection == crate::collection::Collection::Schedule {
            node.add_dac_actor_relationship_as(&context, "Schedule", &doc_id, "reader", editor.did()).await?;
            let editor_access = super::ConfigAccess::Graphql(
                super::GraphqlEndpoint::as_principal(&url, editor.did())
                    .with_client_signing_capability(editor.client_signing_capability()?)?
                    .with_client_signed_agent_creates(),
            );
            let denied = editor_access.transact_write("test.schedule.denied_delete", |txn| {
                let doc_id = &doc_id;
                Box::pin(async move { txn.delete_configuration_document("Schedule", doc_id).await })
            }).await;
            assert!(denied.is_err(), "reader cannot delete Schedule");
            let rolled_back = access.transact_write("test.schedule.rollback", |txn| {
                let doc_id = &doc_id;
                Box::pin(async move {
                    txn.delete_configuration_document("Schedule", doc_id).await?;
                    Err::<(), anyhow::Error>(anyhow::anyhow!("rollback requested"))
                })
            }).await;
            assert!(rolled_back.is_err());
            let retained = access.execute("{ Schedule { _docID } }").await?;
            assert_eq!(retained["data"]["Schedule"][0]["_docID"], doc_id);
            access.transact_write("test.schedule.delete", |txn| {
                let doc_id = &doc_id;
                Box::pin(async move { txn.delete_configuration_document("Schedule", doc_id).await })
            }).await?;
            let deleted = access.execute("{ Schedule { _docID } }").await?;
            assert!(deleted["data"]["Schedule"].as_array().is_some_and(Vec::is_empty));
        }
        if collection == crate::collection::Collection::AgentContext {
            node.add_dac_actor_relationship_as(&context, "AgentContext", &doc_id, "reader", editor.did()).await?;
            let editor_access = super::ConfigAccess::Graphql(
                super::GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), editor.did())
                    .with_client_signing_capability(editor.client_signing_capability()?)?
                    .with_client_signed_agent_creates(),
            );
            let editor_patch = json!({"system_prompt": "agent edit"});
            let edit = || editor_access.transact_write("test.agent_configuration_update", |txn| {
                let doc_id = &doc_id;
                let editor_patch = &editor_patch;
                Box::pin(async move {
                    txn.execute_desired_state_mutation(update, &json!({"id": doc_id, "input": editor_patch}), collection, false).await
                })
            });
            assert!(edit().await.is_err(), "reader must not update");
            node.add_dac_actor_relationship_as(&context, "AgentContext", &doc_id, "writer", editor.did()).await?;
            let edited = edit().await?;
            let cid = edited["data"]["update_AgentContext"][0]["_version"][0]["cid"].as_str().unwrap();
            let (block, signature) = node.authorized_signed_block_bytes(cid, Some(editor.did())).await?;
            let block = defra_core::block::Block::from_dag_cbor(&block)?;
            let signature = defra_core::block::Signature::from_dag_cbor(&signature)?;
            assert_eq!(db::block::verify::verified_signature_signer_did(&block, &signature).map_err(anyhow::Error::msg)?, editor.did());
            node.delete_dac_actor_relationship_as(&context, "AgentContext", &doc_id, "writer", editor.did()).await?;
            assert!(edit().await.is_err(), "revoked writer must not update");
        }
    }
    node.shutdown().await?;
    Ok(())
}
