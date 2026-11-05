use super::client_signed_agent::{create_route, CreateRoute};

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
    use crate::lean_vocab_test::{
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
        expected: &crate::lean_vocab_test::LeanIdentityClientAgentJournalState,
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
    for case in
        &crate::lean_vocab_test::lean_contract_snapshot().identity_client_agent_journal_cases
    {
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
    let cases =
        &crate::lean_vocab_test::lean_contract_snapshot().identity_client_agent_creation_cases;
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

#[tokio::test]
async fn modeled_missing_client_signer_fails_before_connecting() {
    let case = crate::lean_vocab_test::lean_contract_snapshot()
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
    use crate::store::identity::NodeIdentity;
    use serde_json::json;
    use std::sync::Arc;

    let directory = tempfile::tempdir()?;
    let actor = crate::store::identity::KeyIdentity::load_or_create(
        directory.path().join("desktop.key"),
        None,
    )?;
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
                    true,
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
