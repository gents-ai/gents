use super::*;
use crate::session::model_test_support::{model_session, model_time};
use gents_protocol::session::SessionArchiveVisibility;

#[test]
fn authorized_archive_transitions_match_executed_model() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot()
        .session_document_cases
        .archive;
    assert!(!cases.is_empty());
    let mut checked = 0;
    let mut checked_restored_generation = false;
    for case in cases.iter().filter(|case| case["authorized"] == true) {
        let before = model_session(&case["before"]);
        let expected = model_session(&case["after"]);
        let now = model_time(&case["now"]);
        assert_eq!(case["limit"].as_u64(), Some(ARCHIVE_REVISION_LIMIT));
        let (decision, actual) = apply_transition(
            before,
            case["expected_revision"].as_u64().unwrap(),
            case["archived"].as_bool().unwrap(),
            now.as_str().unwrap(),
        );
        assert_eq!(serde_json::to_value(decision).unwrap(), case["decision"]);
        assert_eq!(actual, expected);
        for (visibility, key) in [
            (SessionArchiveVisibility::Active, "active_visible"),
            (SessionArchiveVisibility::Archived, "archived_visible"),
            (SessionArchiveVisibility::All, "all_visible"),
        ] {
            assert_eq!(visibility.includes(actual.archived_at.is_some()), case[key]);
        }
        checked_restored_generation |= case["before"]["archive_revision"] == 2
            && case["expected_revision"] == 0
            && case["archived"] == true;
        checked += 1;
    }
    assert!(checked > 0 && checked_restored_generation);
}

async fn key(home: &std::path::Path, name: &str) -> crate::store::identity::KeyIdentity {
    let path = home.join(format!("{name}.key"));
    tokio::task::spawn_blocking(move || {
        crate::store::identity::KeyIdentity::load_or_create(path, None)
    })
    .await
    .unwrap()
    .unwrap()
}

/// Archive writes by a non-node actor need its managed Agent signer, so the
/// authority fixture's writers are Agents provisioned from the runtime root.
fn managed_agent(runtime: &crate::store::identity::KeyIdentity) -> Arc<dyn NodeIdentity> {
    let binding = crate::store::identity::provision_agent_identity(runtime).unwrap();
    crate::store::identity::resolve_agent_identity(runtime, &binding).unwrap()
}

async fn read_as(node: &Arc<EmbeddedNode>, actor: &dyn NodeIdentity) -> Result<AgentSession> {
    ConfigAccess::transact_local_readonly(
        node,
        Some(::identity::Did::new(actor.did())?),
        "test.archive.read",
        |txn| {
            Box::pin(async move {
                let rows = txn
                    .cursor_rows(
                        "AgentSession",
                        gents_protocol::session::AGENT_SESSION_FIELDS,
                        "",
                    )
                    .await?;
                anyhow::ensure!(rows.len() == 1, "expected one authorized session");
                Ok(gents_protocol::session::decode_session_row(&rows[0])?.session)
            })
        },
    )
    .await
}

/// AgentSession is governed by the Agent ownership policy, so its signed block
/// is read as the user who wrote it; the node identity holds no session relation.
async fn signer_as_actor(node: &EmbeddedNode, actor: &str, cid: &str) -> Result<String> {
    let (block, signature) = node.authorized_signed_block_bytes(cid, Some(actor)).await?;
    let block = defra_core::block::Block::from_dag_cbor(&block)?;
    let signature = defra_core::block::Signature::from_dag_cbor(&signature)?;
    db::block::verify::verified_signature_signer_did(&block, &signature).map_err(anyhow::Error::msg)
}

async fn create_session(node: &Arc<EmbeddedNode>, actor: &dyn NodeIdentity) -> Result<String> {
    ConfigAccess::transact_local_write(
        node, Some(::identity::Did::new(actor.did())?), "test.archive.create",
        |txn| Box::pin(async move {
            let result = txn.execute(r#"mutation { create_AgentSession(input: {
                session_id: "archive-session", node_did: "runtime", requester_did: "other-requester",
                agent_did: "engineer", created_at: "2026-01-01T00:00:00Z",
                tags: ["keep"], title: {text: "Keep this title", source: "user"},
                provenance: {task_id: "original-task"},
                observation: {last_activity_at: "2026-01-02T00:00:00Z", preview: "working",
                    latest_request: {request_doc_id: "physical", request_id: "running",
                        lifecycle_state: "processing"}}
            }) {_docID} }"#).await?;
            Ok::<_, anyhow::Error>(crate::store::graphql::created_doc_id(&result, "AgentSession")?)
        }),
    ).await
}

async fn create_work(node: &Arc<EmbeddedNode>, actor: &dyn NodeIdentity) {
    let physical = ConfigAccess::transact_local_write(
        node, Some(::identity::Did::new(actor.did()).unwrap()), "test.archive.work",
        |txn| Box::pin(async move {
            let value = txn.execute(r#"mutation { create_AgentRequest(input: {
                request_id: "running", purpose: "normal", node_did: "runtime",
                requester_did: "other-requester", session_id: "archive-session",
                agent_did: "engineer", lifecycle_state: "processing", content: "Original input",
                created_at: "2026-01-01T00:00:00Z"
            }) {_docID} }"#).await?;
            let physical = crate::store::graphql::created_doc_id(&value, "AgentRequest")?;
            txn.execute(&format!(r#"mutation {{ update_AgentSession(
                filter: {{session_id: {{_eq: "archive-session"}}, node_did: {{_eq: "runtime"}}}},
                input: {{observation: {{last_activity_at: "2026-01-02T00:00:00Z", preview: "working",
                    latest_request: {{request_doc_id: "{}", request_id: "running", lifecycle_state: "processing"}}}}}}
            ) {{_docID}} }}"#, escape_graphql_string(&physical))).await?;
            Ok::<_, anyhow::Error>(physical)
        }),
    ).await.unwrap();
    crate::session::import_history_observation(
        node,
        &physical,
        "archive-session",
        "runtime",
        Some("other-requester"),
        "Original transcript",
        "archive:original",
        1,
        None,
    )
    .await;
}

async fn work_rows(node: &Arc<EmbeddedNode>, actor: &dyn NodeIdentity) -> serde_json::Value {
    ConfigAccess::transact_local_readonly(
        node,
        Some(::identity::Did::new(actor.did()).unwrap()),
        "test.archive.work_read",
        |txn| {
            Box::pin(async move {
                let value = txn
                    .execute(&format!(
                        "{{ AgentRequest {{ _docID request_id lifecycle_state content }} \
                AgentMessage {{ {} }} AgentOutputSegment {{ {} }} }}",
                        gents_protocol::output::rows::AGENT_MESSAGE_FIELDS,
                        gents_protocol::output::rows::AGENT_OUTPUT_SEGMENT_FIELDS
                    ))
                    .await?;
                Ok::<_, anyhow::Error>(value["data"].clone())
            })
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn archive_restore_survive_reopen_and_reject_stale_windows_without_stopping_work() {
    let home = tempfile::tempdir().unwrap();
    let actor = key(home.path(), "actor").await;
    let data = home.path().join("data");
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(&data)
            .with_node_identity_did(actor.did())
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    create_session(&node, &actor).await.unwrap();
    create_work(&node, &actor).await;
    let work = work_rows(&node, &actor).await;
    let before = read_as(&node, &actor).await.unwrap();
    let archived = set_session_archived_on(&ConfigAccess::local(Arc::clone(&node)), "runtime", "archive-session", &actor, 0, true)
        .await
        .unwrap();
    assert_eq!(archived.outcome, SessionArchiveOutcome::Applied);
    assert_eq!(archived.revision, "1");
    let mut expected = before.clone();
    expected.archived_at = archived.archived_at.clone();
    expected.archive_revision = 1;
    assert_eq!(read_as(&node, &actor).await.unwrap(), expected);
    assert_eq!(work_rows(&node, &actor).await, work);
    // A lost receipt may be retried without rewriting its timestamp or revision.
    assert_eq!(
        set_session_archived_on(&ConfigAccess::local(Arc::clone(&node)), "runtime", "archive-session", &actor, 0, true)
            .await
            .unwrap()
            .outcome,
        SessionArchiveOutcome::Unchanged
    );
    assert_eq!(read_as(&node, &actor).await.unwrap(), expected);
    assert_eq!(work_rows(&node, &actor).await, work);
    node.shutdown().await.expect("shut down test node");
    drop(node);
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(&data)
            .with_node_identity_did(actor.did())
            .build()
            .await
            .unwrap(),
    );
    assert_eq!(read_as(&node, &actor).await.unwrap(), expected);
    assert_eq!(work_rows(&node, &actor).await, work);
    let other_window = Arc::clone(&node);
    assert_eq!(
        set_session_archived_on(&ConfigAccess::local(Arc::clone(&other_window)),
            "runtime",
            "archive-session",
            &actor,
            0,
            false
        )
        .await
        .unwrap()
        .outcome,
        SessionArchiveOutcome::Conflict
    );
    let restored = set_session_archived_on(&ConfigAccess::local(Arc::clone(&node)), "runtime", "archive-session", &actor, 1, false)
        .await
        .unwrap();
    assert_eq!(restored.outcome, SessionArchiveOutcome::Applied);
    expected = before;
    expected.archive_revision = 2;
    assert_eq!(read_as(&other_window, &actor).await.unwrap(), expected);
    assert_eq!(work_rows(&other_window, &actor).await, work);
    assert_eq!(
        set_session_archived_on(&ConfigAccess::local(Arc::clone(&other_window)), "runtime", "archive-session", &actor, 0, true)
            .await
            .unwrap()
            .outcome,
        SessionArchiveOutcome::Conflict
    );
    assert!(set_session_archived_on(&ConfigAccess::local(Arc::clone(&node)),
        "different-runtime",
        "archive-session",
        &actor,
        2,
        true
    )
    .await
    .is_err());
    assert_eq!(read_as(&node, &actor).await.unwrap(), expected);
    assert_eq!(work_rows(&node, &actor).await, work);
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn archive_authority_is_native_acp_and_never_requester_scope() {
    let home = tempfile::tempdir().unwrap();
    let owner = key(home.path(), "owner").await;
    let writer = managed_agent(&owner);
    let reader = managed_agent(&owner);
    let denied = managed_agent(&owner);
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(owner.did())
            .build()
            .await
            .unwrap(),
    );
    let policy = node
        .add_dac_policy_as(
            &defra_core::RequestContext::default().with_identity(Some(owner.did().to_owned())),
            r#"
name: Archive authority
resources:
  - name: sessions
    relations:
      - name: reader
      - name: writer
    permissions:
      - name: read
        expr: reader + writer
      - name: update
        expr: writer
      - name: delete
"#,
        )
        .await
        .unwrap();
    let schema = gents_protocol::schemas::AGENT_SESSION.replace(
        &format!(
            "@policy(id: \"{}\", resource: \"sessions\")",
            gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID
        ),
        &format!("@policy(id: \"{policy}\", resource: \"sessions\")"),
    );
    ConfigAccess::local(Arc::clone(&node))
        .add_collection(&schema)
        .await
        .unwrap();
    let id = create_session(&node, &owner).await.unwrap();
    let access = ConfigAccess::local_as(node.clone(), ::identity::Did::new(owner.did()).unwrap());
    access
        .add_actor_relationship(
            "AgentSession",
            &id,
            "reader",
            &::identity::Did::new(reader.did()).unwrap(),
        )
        .await
        .unwrap();
    access
        .add_actor_relationship(
            "AgentSession",
            &id,
            "writer",
            &::identity::Did::new(writer.did()).unwrap(),
        )
        .await
        .unwrap();
    let before = read_as(&node, &owner).await.unwrap();
    assert!(
        set_session_archived_on(&ConfigAccess::local(Arc::clone(&node)), "runtime", "archive-session", &*denied, 0, true)
            .await
            .is_err()
    );
    assert!(read_as(&node, &*denied).await.is_err());
    assert!(
        set_session_archived_on(&ConfigAccess::local(Arc::clone(&node)), "runtime", "archive-session", &*reader, 0, true)
            .await
            .is_err()
    );
    assert_eq!(read_as(&node, &owner).await.unwrap(), before);
    let result = set_session_archived_on(&ConfigAccess::local(Arc::clone(&node)), "runtime", "archive-session", &*writer, 0, true)
        .await
        .unwrap();
    assert_eq!(result.outcome, SessionArchiveOutcome::Applied);
    assert_eq!(
        read_as(&node, &*reader).await.unwrap().archived_at,
        result.archived_at
    );
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn concurrent_archive_calls_commit_one_revision() {
    let home = tempfile::tempdir().unwrap();
    let actor = key(home.path(), "owner").await;
    // The archive writer is the runtime owner, so the node identity is the actor.
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(actor.did())
            .build()
            .await
            .unwrap(),
    );
    crate::test_support::acp_policies::install_identity_acp_policies(&node)
        .await
        .unwrap();
    node.add_collection(gents_protocol::schemas::AGENT_SESSION)
        .await
        .unwrap();
    create_session(&node, &actor).await.unwrap();
    let ready = tokio::sync::Barrier::new(16);
    let calls = (0..16).map(|_| async {
        ready.wait().await;
        set_session_archived_on(&ConfigAccess::local(Arc::clone(&node)), "runtime", "archive-session", &actor, 0, true)
            .await
            .unwrap()
    });
    let receipts = futures::future::join_all(calls).await;
    assert_eq!(
        receipts
            .iter()
            .filter(|receipt| receipt.outcome == SessionArchiveOutcome::Applied)
            .count(),
        1
    );
    assert!(receipts.iter().all(|receipt| receipt.revision == "1"));
    assert_eq!(read_as(&node, &actor).await.unwrap().archive_revision, 1);
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn authenticated_http_archive_uses_the_same_durable_transaction_owner() -> Result<()> {
    let home = tempfile::tempdir().unwrap();
    let user = key(home.path(), "user").await;
    let runtime = key(home.path(), "runtime").await;
    let reader = key(home.path(), "reader").await;
    let binding = crate::store::identity::provision_agent_identity(&runtime)?;
    let agent = crate::store::identity::resolve_agent_identity(&runtime, &binding)?;
    let worker_binding = crate::store::identity::provision_agent_identity(&runtime)?;
    let worker = crate::store::identity::resolve_agent_identity(&runtime, &worker_binding)?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let endpoint = format!("http://{}/api/v0/graphql", listener.local_addr()?);
    let node = Arc::new(
        EmbeddedNode::builder()
            .enable_signing()
            .with_node_identity_did(runtime.did())
            .with_node_acp_owner(user.did().to_owned())
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .build()
            .await?,
    );
    let context = defra_core::RequestContext::default().with_identity(Some(user.did().to_owned()));
    let policy = node
        .add_dac_policy_with_counter_as(
            &context,
            gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
            0,
        )
        .await?;
    assert_eq!(policy, gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID);
    for schema in [
        gents_protocol::schemas::NODE,
        gents_protocol::schemas::AGENT,
        gents_protocol::schemas::AGENT_SESSION,
    ] {
        node.add_collection_as(&context, schema).await?;
    }
    for actor in [agent.did(), worker.did(), reader.did()] {
        for permission in [
            acp::nac::NodePermission::DocumentRead,
            acp::nac::NodePermission::DocumentUpdate,
            acp::nac::NodePermission::CollectionGet,
            acp::nac::NodePermission::DacRelationAdd,
        ] {
            node.add_nac_actor_relationship_as(&context, permission.as_str(), actor)
                .await?;
        }
    }
    let client = |identity: &crate::KeyIdentity| -> Result<ConfigAccess> {
        Ok(ConfigAccess::Graphql(
            crate::store::config_client::GraphqlEndpoint::as_principal(&endpoint, identity.did())
                .with_client_signing_capability(identity.client_signing_capability()?)?
                .with_client_signed_agent_creates(),
        ))
    };
    let access = client(&user)?;
    let node_doc = access
        .transact_write("test.archive.node", |txn| {
            Box::pin(async {
                txn.execute_client_signed_create(
                "mutation($input: NodeMutationInputArg!) { create_Node(input: $input) { _docID } }",
                &serde_json::json!({"input": {"node_did": runtime.did()}}),
                gents_protocol::schemas::NODE,
            ).await
            })
        })
        .await?;
    let node_doc = crate::store::graphql::created_doc_id(&node_doc, "Node")?;
    let agent_doc = access.transact_write("test.archive.agent", |txn| {
        Box::pin(async {
            txn.execute_client_signed_create(
                "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID } }",
                &serde_json::json!({"input": {"node_did": runtime.did(), "agent_did": worker.did(), "creation_nonce": worker_binding.creation_nonce}}),
                gents_protocol::schemas::AGENT,
            ).await
        })
    }).await?;
    let agent_doc = crate::store::graphql::created_doc_id(&agent_doc, "Agent")?;
    access
        .add_resource_relationship("Agent", &agent_doc, "parent", "nodes", &node_doc)
        .await?;
    let creator = ConfigAccess::local_managed_agent(
        node.clone(),
        ::identity::Did::new(agent.did())?,
        agent.as_ref(),
    )?;
    let created = creator.write("test.archive.session", &format!(
        r#"mutation {{ create_AgentSession(input: {{session_id: "archive-session", node_did: "{}", requester_did: "{}", agent_did: "{}", created_at: "2026-01-01T00:00:00Z"}}) {{ _docID }} }}"#,
        escape_graphql_string(runtime.did()), escape_graphql_string(agent.did()), escape_graphql_string(worker.did()),
    )).await?;
    let session_doc = crate::store::graphql::created_doc_id(&created, "AgentSession")?;
    creator
        .add_resource_relationship("AgentSession", &session_doc, "parent", "agents", &agent_doc)
        .await?;
    creator
        .add_actor_relationship(
            "AgentSession",
            &session_doc,
            "working_agent",
            &::identity::Did::new(worker.did())?,
        )
        .await?;
    creator
        .add_actor_relationship(
            "AgentSession",
            &session_doc,
            "reader",
            &::identity::Did::new(reader.did())?,
        )
        .await?;
    let owners = || async {
        creator
            .transact_readonly("test.archive.owner", |txn| {
                let session_doc = &session_doc;
                Box::pin(async move {
                    txn.document_owners("AgentSession", std::slice::from_ref(session_doc))
                        .await
                })
            })
            .await
    };
    assert_eq!(
        owners().await?[0]
            .owner
            .as_ref()
            .map(|owner| owner.as_str()),
        Some(agent.did())
    );
    let denied = client(&reader)?;
    assert!(
        set_session_archived_on(&denied, runtime.did(), "archive-session", &reader, 0, true)
            .await
            .is_err()
    );
    assert_eq!(read_as(&node, &user).await?.archive_revision, 0);
    let unsigned = ConfigAccess::Graphql(
        crate::store::config_client::GraphqlEndpoint::as_principal(&endpoint, user.did()),
    );
    assert!(
        set_session_archived_on(&unsigned, runtime.did(), "archive-session", &user, 0, true)
            .await
            .is_err()
    );
    let receipt =
        set_session_archived_on(&access, runtime.did(), "archive-session", &user, 0, true).await?;
    assert_eq!(receipt.outcome, SessionArchiveOutcome::Applied);
    assert_eq!(receipt.revision, "1");
    assert_eq!(
        read_as(&node, worker.as_ref()).await?.archived_at,
        receipt.archived_at
    );
    assert_eq!(
        read_as(&node, &reader).await?.archived_at,
        receipt.archived_at
    );
    let session = access
        .execute(&format!(
            r#"{{ AgentSession(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _version {{ cid }} }} }}"#,
            escape_graphql_string(&session_doc)
        ))
        .await?;
    let cid = session["data"]["AgentSession"][0]["_version"][0]["cid"]
        .as_str()
        .context("archive update missing version")?;
    assert_eq!(signer_as_actor(&node, user.did(), cid).await?, user.did());
    assert_eq!(
        owners().await?[0]
            .owner
            .as_ref()
            .map(|owner| owner.as_str()),
        Some(agent.did())
    );
    assert_eq!(
        set_session_archived_on(&access, runtime.did(), "archive-session", &user, 0, false)
            .await?
            .outcome,
        SessionArchiveOutcome::Conflict
    );
    let receipt =
        set_session_archived_on(&access, runtime.did(), "archive-session", &user, 1, false).await?;
    assert_eq!(receipt.outcome, SessionArchiveOutcome::Applied);
    assert_eq!(read_as(&node, &user).await?.archive_revision, 2);
    assert_eq!(
        owners().await?[0]
            .owner
            .as_ref()
            .map(|owner| owner.as_str()),
        Some(agent.did())
    );
    node.shutdown().await?;
    Ok(())
}
