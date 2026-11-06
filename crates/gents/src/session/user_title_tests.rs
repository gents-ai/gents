use std::sync::Arc;

use super::model_test_support::{model_session, model_time};
use super::observations::apply_title_in_txn;
use super::query::load_agent_session;
use gents_protocol::session::{SessionTitle, SessionTitleSource};

async fn verified_signer_as_user(
    node: &defra_node::Node,
    cid: &str,
    user_did: &str,
) -> anyhow::Result<String> {
    let (block, signature) = node
        .authorized_signed_block_bytes(cid, Some(user_did))
        .await?;
    let block = defra_core::block::Block::from_dag_cbor(&block)?;
    let signature = defra_core::block::Signature::from_dag_cbor(&signature)?;
    db::block::verify::verified_signature_signer_did(&block, &signature).map_err(anyhow::Error::msg)
}

#[tokio::test]
async fn user_title_update_is_signed_on_authenticated_http() -> anyhow::Result<()> {
    use crate::store::identity::NodeIdentity;
    let case = crate::lean_vocab_test::lean_contract_snapshot()
        .session_document_cases
        .projection
        .iter()
        .find(|case| case["operation"] == "rename" && case["title"]["text"].is_string())
        .expect("modeled user rename");
    let title = case["title"]["text"].as_str().unwrap();
    let (db, user, access, binding) =
        crate::test_support::signed_test_node("http-user-title").await;
    let node_did = db.node_identity.did();
    crate::request::lifecycle::build_signed_client_request(
        &access,
        crate::request::lifecycle::RequestSpec::new(
            gents_protocol::request_admission::RequestPurpose::Normal,
            crate::request::lifecycle::RequestIdentity {
                request_id: "http-title-request".into(),
                node_did: node_did.into(),
                requester_did: Some(user.did().into()),
                agent_did: binding.agent_did,
                session_id: "http-title-session".into(),
                content: "work".into(),
                execution_origin: crate::request::lifecycle::ExecutionOrigin::Interactive,
                created_at: "2026-09-22T00:00:00Z".into(),
            },
            gents_protocol::request_admission::AgentRequestAdmissionRecord::client(user.did()),
        ),
        crate::request::lifecycle::RequestSigner::Identity(&user),
    )
    .await?;
    let user_did = user.did().to_owned();
    access
        .transact_write("test.http_user_title", |txn| {
            let user_did = &user_did;
            Box::pin(async move {
                apply_title_in_txn(
                    txn,
                    node_did,
                    Some(user_did),
                    "http-title-session",
                    Some(title),
                    SessionTitleSource::User,
                    "2026-09-23T00:00:00Z",
                )
                .await
            })
        })
        .await?;
    let response = access.execute(
        r#"{ AgentSession(filter: {session_id: {_eq: "http-title-session"}}) {title _version {cid}} }"#,
    ).await?;
    let row = &response["data"]["AgentSession"][0];
    assert_eq!(row["title"], case["after"]["title"]);
    let cid = row["_version"][0]["cid"]
        .as_str()
        .expect("signed title version");
    assert_eq!(
        verified_signer_as_user(&db.node, cid, user.did()).await?,
        user.did()
    );
    db.node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn user_rename_rebuilds_after_confirmed_http_commit_conflict() -> anyhow::Result<()> {
    use crate::store::config_client::{
        ConfigAccess, ConfigApplyTxn, GraphqlEndpoint, NodePermission,
    };
    use crate::store::identity::NodeIdentity;
    use anyhow::Context;

    let (db, user, access, binding) =
        crate::test_support::signed_test_node("http-user-title-conflict").await;
    let node_did = db.node_identity.did();
    let session_id = "http-title-conflict-session";
    crate::request::lifecycle::build_signed_client_request(
        &access,
        crate::request::lifecycle::RequestSpec::new(
            gents_protocol::request_admission::RequestPurpose::Normal,
            crate::request::lifecycle::RequestIdentity {
                request_id: "http-title-conflict-request".into(),
                node_did: node_did.into(),
                requester_did: Some(user.did().into()),
                agent_did: binding.agent_did,
                session_id: session_id.into(),
                content: "work".into(),
                execution_origin: crate::request::lifecycle::ExecutionOrigin::Interactive,
                created_at: "2026-09-22T00:00:00Z".into(),
            },
            gents_protocol::request_admission::AgentRequestAdmissionRecord::client(user.did()),
        ),
        crate::request::lifecycle::RequestSigner::Identity(&user),
    )
    .await
    .context("creating signed session")?;

    let reader = tokio::task::spawn_blocking(|| {
        crate::support::fixtures::test_identity("http-user-title-reader")
    })
    .await?;
    let reader_did = identity::Did::new(reader.did())?;
    let admin = ConfigAccess::local_as(db.node.clone(), identity::Did::new(node_did)?);
    for permission in [
        NodePermission::DocumentRead,
        NodePermission::DocumentUpdate,
        NodePermission::CollectionGet,
    ] {
        admin
            .grant_node_permission(&reader_did, permission)
            .await
            .with_context(|| format!("granting reader node permission {permission:?}"))?;
    }
    let before = access.execute(
        r#"{ AgentSession(filter: {session_id: {_eq: "http-title-conflict-session"}}) {_docID title} }"#,
    ).await.context("reading session before denied rename")?;
    let doc = before["data"]["AgentSession"][0]["_docID"]
        .as_str()
        .unwrap();
    access
        .add_actor_relationship("AgentSession", doc, "reader", &reader_did)
        .await
        .context("granting session reader relationship")?;
    let reader_access = db.client_access(&reader);
    let visible = reader_access.execute(
        r#"{ AgentSession(filter: {session_id: {_eq: "http-title-conflict-session"}}) {_docID} }"#,
    ).await.context("reading session as reader")?;
    assert_eq!(visible["data"]["AgentSession"][0]["_docID"], doc);
    let denied = super::rename_session_on(
        &reader_access,
        node_did,
        user.did(),
        session_id,
        "unauthorized",
    )
    .await;
    assert!(
        denied.is_err(),
        "a reader must not acquire update permission"
    );
    let after_denial = access.execute(
        r#"{ AgentSession(filter: {session_id: {_eq: "http-title-conflict-session"}}) {_docID title} }"#,
    ).await.context("reading session after denied rename")?;
    assert_eq!(
        after_denial["data"]["AgentSession"],
        before["data"]["AgentSession"]
    );

    let missing_signer = ConfigAccess::Graphql(GraphqlEndpoint::as_principal(
        db.client_endpoint(),
        user.did(),
    ));
    let error = super::rename_session_on(
        &missing_signer,
        node_did,
        user.did(),
        session_id,
        "untrusted",
    )
    .await
    .unwrap_err();
    assert!(
        error.chain().any(|cause| matches!(
            cause.downcast_ref::<crate::store::config_client::ConfigClientError>(),
            Some(crate::store::config_client::ConfigClientError::ClientAgentSignerRequired)
        )),
        "unexpected missing-signer error: {error:?}; typed causes: {:?}",
        error
            .chain()
            .filter_map(
                |cause| cause.downcast_ref::<crate::store::config_client::ConfigClientError>()
            )
            .collect::<Vec<_>>()
    );
    let error =
        super::rename_session_on(&access, node_did, user.did(), "missing-session", "missing")
            .await
            .unwrap_err();
    assert!(error.to_string().contains("no AgentSession"), "{error:#}");

    let reached = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut rename = Box::pin(ConfigApplyTxn::with_successful_mutation_pause_and_count(
        1,
        reached.clone(),
        release.clone(),
        super::rename_session_on(&access, node_did, user.did(), session_id, "final title"),
    ));
    tokio::select! {
        () = reached.notified() => {}
        result = &mut rename => panic!("rename finished before the staged update: {result:?}"),
    }
    super::rename_session_on(
        &access,
        node_did,
        user.did(),
        session_id,
        "concurrent title",
    )
    .await
    .context("committing competing rename")?;
    release.notify_one();
    let (result, paused, updates) = rename.await;
    assert!(paused);
    result.context("committing and rebuilding paused rename")?;
    assert_eq!(updates, 2, "the aborted signed update must be rebuilt once");

    let response = access.execute(
        r#"{ AgentSession(filter: {session_id: {_eq: "http-title-conflict-session"}}) {title _version {cid}} }"#,
    ).await.context("reading final renamed session")?;
    let row = &response["data"]["AgentSession"][0];
    assert_eq!(row["title"]["text"], "final title");
    let cid = row["_version"][0]["cid"].as_str().expect("title version");
    assert_eq!(
        verified_signer_as_user(&db.node, cid, user.did())
            .await
            .context("verifying title signer")?,
        user.did()
    );
    db.node
        .shutdown()
        .await
        .context("shutting down test node")?;
    Ok(())
}

#[tokio::test]
async fn title_transitions_preserve_session_and_request_observation() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::test_support::acp_policies::install_identity_acp_policies(&node).await.unwrap();
    node.add_collection(gents_protocol::schemas::AGENT_SESSION)
        .await
        .unwrap();
    let response = node.execute(r#"mutation { create_AgentSession(input: {
            session_id: "title-session", node_did: "owner", agent_did: "agent",
            created_at: "2026-01-01T00:00:00Z", closed_at: "2026-01-02T00:00:00Z",
            title: {text: "task title", source: "task"}, tags: ["important"],
            provenance: {task_id: "task"},
            observation: {last_activity_at: "2026-01-03T00:00:00Z", preview: "keep this preview",
                latest_request: {request_doc_id: "physical-request", request_id: "request", lifecycle_state: "completed"}}
        }) {_docID} }"#).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let before = load_agent_session(&node, "owner", "title-session", None)
        .await
        .unwrap()
        .unwrap();
    for source in [
        SessionTitleSource::Generated,
        SessionTitleSource::Task,
        SessionTitleSource::Placeholder,
    ] {
        crate::store::config_client::ConfigAccess::transact_local_write(
            &node,
            None,
            "test.automatic_title",
            |txn| {
                Box::pin(async move {
                    apply_title_in_txn(
                        txn,
                        "owner",
                        None,
                        "title-session",
                        Some("automatic"),
                        source,
                        "2026-01-04T00:00:00Z",
                    )
                    .await
                })
            },
        )
        .await
        .unwrap();
        assert_eq!(
            load_agent_session(&node, "owner", "title-session", None)
                .await
                .unwrap()
                .unwrap(),
            before
        );
    }
    crate::store::config_client::ConfigAccess::transact_local_write(
        &node,
        None,
        "test.user_title",
        |txn| {
            Box::pin(async move {
                apply_title_in_txn(
                    txn,
                    "owner",
                    None,
                    "title-session",
                    Some("  renamed  "),
                    SessionTitleSource::User,
                    "2026-01-02T00:00:00Z",
                )
                .await
            })
        },
    )
    .await
    .unwrap();
    let renamed = load_agent_session(&node, "owner", "title-session", None)
        .await
        .unwrap()
        .unwrap();
    let mut expected = before.clone();
    expected.title = Some(SessionTitle {
        text: "renamed".into(),
        source: SessionTitleSource::User,
    });
    assert_eq!(renamed, expected);
    crate::store::config_client::ConfigAccess::transact_local_write(
        &node,
        None,
        "test.clear_title",
        |txn| {
            Box::pin(async move {
                apply_title_in_txn(
                    txn,
                    "owner",
                    None,
                    "title-session",
                    None,
                    SessionTitleSource::User,
                    "2026-01-04T00:00:00Z",
                )
                .await
            })
        },
    )
    .await
    .unwrap();
    expected.title = None;
    expected.observation.as_mut().unwrap().last_activity_at = "2026-01-04T00:00:00Z".into();
    assert_eq!(
        load_agent_session(&node, "owner", "title-session", None)
            .await
            .unwrap()
            .unwrap(),
        expected
    );

    let response = node.execute(r#"mutation { create_AgentSession(input: {
            session_id: "generated-title-session", node_did: "owner", agent_did: "agent",
            created_at: "2026-01-01T00:00:00Z", closed_at: "2026-01-02T00:00:00Z",
            title: {text: "New session", source: "placeholder"}, tags: ["important"],
            provenance: {task_id: "task"},
            observation: {last_activity_at: "2026-01-03T00:00:00Z", preview: "keep this preview",
                latest_request: {request_doc_id: "physical-request", request_id: "request", lifecycle_state: "completed"}}
        }) {_docID} }"#).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let placeholder = load_agent_session(&node, "owner", "generated-title-session", None)
        .await
        .unwrap()
        .unwrap();
    crate::store::config_client::ConfigAccess::transact_local_write(
        &node,
        None,
        "test.generated_title",
        |txn| {
            Box::pin(async move {
                apply_title_in_txn(
                    txn,
                    "owner",
                    None,
                    "generated-title-session",
                    Some("generated title"),
                    SessionTitleSource::Generated,
                    "2026-01-04T00:00:00Z",
                )
                .await
            })
        },
    )
    .await
    .unwrap();
    let generated = load_agent_session(&node, "owner", "generated-title-session", None)
        .await
        .unwrap()
        .unwrap();
    let mut expected_generated = placeholder;
    expected_generated.title = Some(SessionTitle {
        text: "generated title".into(),
        source: SessionTitleSource::Generated,
    });
    expected_generated
        .observation
        .as_mut()
        .unwrap()
        .last_activity_at = "2026-01-04T00:00:00Z".into();
    assert_eq!(generated, expected_generated);
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn generated_user_title_cases_drive_the_shared_session_owner() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot()
        .session_document_cases
        .projection;
    let cases = cases
        .iter()
        .filter(|case| case["operation"] == "rename")
        .collect::<Vec<_>>();
    assert!(cases.iter().any(|case| case["title"].is_null()));
    assert!(cases
        .iter()
        .any(|case| !case["before"]["title"].is_null() && !case["title"].is_null()));
    for case in cases {
        let before = model_session(&case["before"]);
        let expected = model_session(&case["after"]);
        let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
        crate::test_support::acp_policies::install_identity_acp_policies(&node).await.unwrap();
        node.add_collection(gents_protocol::schemas::AGENT_SESSION)
            .await
            .unwrap();
        crate::store::config_client::ConfigAccess::transact_local_write(&node, None, "test.seed_model_title", |txn| {
                let before = before.clone();
                Box::pin(async move {
                    txn.execute_with_variables(
                        "mutation($input: AgentSessionMutationInputArg!) { create_AgentSession(input: $input) {_docID} }",
                        &serde_json::json!({"input":before}),
                    ).await?;
                    Ok::<_, anyhow::Error>(())
                })
            }).await.unwrap();
        let now = model_time(&case["now"]).as_str().unwrap().to_owned();
        let title = case["title"]["text"].as_str();
        crate::store::config_client::ConfigAccess::transact_local_write(
            &node,
            None,
            "test.model_user_title",
            |txn| {
                let before = before.clone();
                let now = now.clone();
                Box::pin(async move {
                    apply_title_in_txn(
                        txn,
                        &before.node_did,
                        before.requester_did.as_deref(),
                        &before.session_id,
                        title,
                        SessionTitleSource::User,
                        &now,
                    )
                    .await
                })
            },
        )
        .await
        .unwrap();
        let after = load_agent_session(
            &node,
            &before.node_did,
            &before.session_id,
            before.requester_did.as_deref(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(after, expected, "Lean user title case {}", case["name"]);
        node.shutdown().await.expect("shut down test node");
    }
}

#[tokio::test]
async fn title_observation_uses_selected_agent_session_access() {
    use crate::store::identity::NodeIdentity;
    use gents_protocol::agent_identity::AgentIdentityBinding;

    async fn reserve(
        db: &crate::support::TestDb,
        user: &crate::KeyIdentity,
        access: &crate::ConfigAccess,
        agent: &AgentIdentityBinding,
        session: &str,
    ) {
        crate::request::lifecycle::build_signed_client_request(
            access,
            crate::request::lifecycle::RequestSpec::new(
                gents_protocol::request_admission::RequestPurpose::Normal,
                crate::request::lifecycle::RequestIdentity {
                    request_id: format!("{session}-request"),
                    node_did: db.node_identity.did().to_owned(),
                    requester_did: Some(user.did().to_owned()),
                    agent_did: agent.agent_did.clone(),
                    session_id: session.to_owned(),
                    content: "work".into(),
                    execution_origin: crate::request::lifecycle::ExecutionOrigin::Interactive,
                    created_at: "2026-09-22T00:00:00Z".into(),
                },
                gents_protocol::request_admission::AgentRequestAdmissionRecord::client(user.did()),
            ),
            crate::request::lifecycle::RequestSigner::Identity(user),
        )
        .await
        .unwrap();
    }

    let (db, user, owner, first) = crate::test_support::signed_test_node("title-observation").await;
    let user_did = user.did().to_owned();
    let second = crate::store::identity::provision_agent_identity_authorized(
        &db.node,
        db.node_identity.as_ref(),
        &identity::Did::new(&user_did).unwrap(),
    )
    .await
    .unwrap();
    crate::test_support::install_test_agent_binding_with_access(
        db.node_identity.did(),
        "Other title agent",
        &second,
        &owner,
    )
    .await;
    let first_identity =
        crate::store::identity::resolve_agent_identity(db.node_identity.as_ref(), &first).unwrap();
    let second_identity =
        crate::store::identity::resolve_agent_identity(db.node_identity.as_ref(), &second).unwrap();
    let first_access = crate::ConfigAccess::local_managed_agent(
        db.node.clone(),
        identity::Did::new(first_identity.did()).unwrap(),
        first_identity.as_ref(),
    )
    .unwrap();
    let second_access = crate::ConfigAccess::local_managed_agent(
        db.node.clone(),
        identity::Did::new(second_identity.did()).unwrap(),
        second_identity.as_ref(),
    )
    .unwrap();
    let runtime_access = crate::ConfigAccess::local_as(
        db.node.clone(),
        identity::Did::new(db.node_identity.did()).unwrap(),
    );
    let node_did = db.node_identity.did();
    for (agent, session) in [
        (&first, "title-target"),
        (&first, "title-recent"),
        (&second, "title-foreign"),
    ] {
        reserve(&db, &user, &owner, agent, session).await;
    }
    for (session, title) in [
        ("title-recent", "Prior title"),
        ("title-foreign", "Foreign title"),
    ] {
        owner
            .transact_write("test.title_observation", |txn| {
                let user_did = &user_did;
                Box::pin(async move {
                    apply_title_in_txn(
                        txn,
                        node_did,
                        Some(user_did.as_str()),
                        session,
                        Some(title),
                        SessionTitleSource::User,
                        "2026-09-23T00:00:00Z",
                    )
                    .await
                })
            })
            .await
            .unwrap();
    }
    assert!(super::observations::session_needs_generated_title(
        &first_access,
        node_did,
        Some(user_did.as_str()),
        "title-target"
    )
    .await
    .unwrap());
    assert!(!super::observations::session_needs_generated_title(
        &runtime_access,
        node_did,
        Some(user_did.as_str()),
        "title-target"
    )
    .await
    .unwrap());
    assert!(!super::observations::session_needs_generated_title(
        &second_access,
        node_did,
        Some(user_did.as_str()),
        "title-target"
    )
    .await
    .unwrap());
    assert_eq!(
        super::observations::load_recent_titles_for_agent(
            &first_access,
            node_did,
            "title-target",
            5,
        )
        .await
        .unwrap(),
        vec!["Prior title"]
    );
    owner
        .transact_write("test.title_observation.rename", |txn| {
            let user_did = &user_did;
            Box::pin(async move {
                apply_title_in_txn(
                    txn,
                    node_did,
                    Some(user_did.as_str()),
                    "title-target",
                    Some("Chosen title"),
                    SessionTitleSource::User,
                    "2026-09-23T00:00:01Z",
                )
                .await
            })
        })
        .await
        .unwrap();
    assert!(!super::observations::session_needs_generated_title(
        &first_access,
        node_did,
        Some(user_did.as_str()),
        "title-target"
    )
    .await
    .unwrap());
    db.node.shutdown().await.expect("node shutdown");
}
