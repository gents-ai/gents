use super::*;
use crate::request::admission::AgentRequestAdmissionVerifier;
use crate::request::lifecycle::{
    ClaimOutcome, ExecutionOrigin, RequestLifecycle, RequestTerminalOutcome, TerminalizeResult,
};
use crate::request::watcher::AgentRequest;
use crate::session::mailbox::{
    MailboxAction, MailboxCreateTool, MailboxKind, MailboxNotificationPolicy, MailboxWriteOutcome,
};
use crate::store::config_client::{GraphqlEndpoint, NodePermission};
use gents_loop::tool::{bind_request_tools, ToolDyn};

#[tokio::test]
async fn delegated_http_claim_routes_mailbox_to_immediate_signer() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let runtime: Arc<dyn NodeIdentity> = Arc::new(KeyIdentity::load_or_create(
        home.path().join("runtime.key"),
        None,
    )?);
    let owner = KeyIdentity::load_or_create(home.path().join("owner.key"), None)?;
    let user = KeyIdentity::load_or_create(home.path().join("user.key"), None)?;
    let outsider = KeyIdentity::load_or_create(home.path().join("other.key"), None)?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let node = Arc::new(
        defra_node::Node::builder()
            .enable_signing()
            .with_node_identity_did(runtime.did())
            .with_node_acp_enabled()
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .build()
            .await?,
    );
    crate::ensure_runtime_schemas(&node).await?;
    let binding = crate::store::identity::provision_agent_identity(runtime.as_ref())?;
    let agent = crate::store::identity::resolve_agent_identity(runtime.as_ref(), &binding)?;
    let admin = ConfigAccess::local_as(node.clone(), runtime.did().parse()?);
    for did in [owner.did(), user.did(), outsider.did(), agent.did()] {
        for permission in [
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::DacRelationAdd,
            NodePermission::DacRelationDelete,
            NodePermission::CollectionGet,
        ] {
            admin
                .grant_node_permission(&did.parse()?, permission)
                .await?;
        }
    }
    let endpoint = format!("http://{address}/api/v0/graphql");
    let client = |key: &KeyIdentity| -> anyhow::Result<ConfigAccess> {
        Ok(ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(&endpoint, key.did())
                .with_client_signing_capability(key.client_signing_capability()?)?,
        ))
    };
    let owner_access = client(&owner)?;
    let user_access = client(&user)?;
    let outsider_access = client(&outsider)?;
    seed_owned_agent(&owner_access, runtime.did(), &binding).await;
    let agent_rows = owner_access.execute("{ Agent { _docID } }").await?;
    let agent_doc = agent_rows["data"]["Agent"][0]["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    owner_access
        .add_actor_relationship("Agent", &agent_doc, "session_writer", &user.did().parse()?)
        .await?;
    let owner_create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "mailbox-owner-seed",
        runtime.did(),
        owner.did(),
        agent.did(),
        "mailbox-delegated-session",
        "Seed",
        "interactive",
        "2026-11-04T00:00:00Z",
        AgentRequestAdmissionRecord::client(owner.did()),
    );
    let (session_doc, selected_agent_doc) =
        reserve_client_targets(&owner_access, &owner_create).await?;
    let mut create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "mailbox-delegated-request",
        runtime.did(),
        owner.did(),
        agent.did(),
        "mailbox-delegated-session",
        "File a notice",
        "interactive",
        "2026-11-04T00:00:00Z",
        AgentRequestAdmissionRecord::client(user.did()),
    );
    assert_eq!(selected_agent_doc, agent_doc);
    create.session_doc_id = Some(session_doc);
    create.agent_doc_id = Some(selected_agent_doc);
    crate::sign_agent_request_create(&user, &mut create).await?;
    submit_client_request(&user_access, &create).await?;
    let agent_access =
        ConfigAccess::local_managed_agent(node.clone(), agent.did().parse()?, agent.as_ref())?;
    let response = agent_access
        .execute(&format!(
            "{{ AgentRequest {{ {} }} }}",
            crate::SIGNED_REQUEST_FIELDS
        ))
        .await?;
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(response["data"]["AgentRequest"][0].clone())?;
    let request = AgentRequest::try_from(row)?;
    let (_authority_owner, authority) =
        crate::runtime::agent::p2p_reconcile::enrollment_authority_channel();
    let verifier = AgentRequestAdmissionVerifier::new(node.clone(), runtime.clone(), authority);
    let (request, admission) = verifier
        .verify_fresh_for_claim(&request, agent.did())
        .await?;
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        agent.did(),
        runtime.did(),
        request,
        60,
        ExecutionOrigin::Interactive,
        String::new(),
    );
    lifecycle.bind_managed_agent(agent.as_ref())?;
    lifecycle.set_client_claim_admission(admission);
    let modeled_claim =
        &crate::lean_vocab_test::lean_contract_snapshot().mailbox_claimed_recipient_cases;
    let before = modeled_claim
        .iter()
        .find(|case| case["present"] == true && case["committed"] == false)
        .unwrap();
    assert_eq!(
        lifecycle.claimed_tool_context().is_ok(),
        !before["signer"].is_null()
    );
    assert_eq!(
        lifecycle.claim_with_identity().await?,
        ClaimOutcome::Claimed
    );
    let context = lifecycle.claimed_tool_context()?;
    let after = modeled_claim
        .iter()
        .find(|case| case["present"] == true && case["committed"] == true)
        .unwrap();
    assert_eq!(
        context.admitted_client.is_some(),
        !after["signer"].is_null()
    );
    assert_eq!(
        context.admitted_client.as_ref().unwrap().signer_did,
        user.did()
    );
    assert_ne!(owner.did(), user.did());
    owner_access
        .delete_actor_relationship("Agent", &agent_doc, "session_writer", &user.did().parse()?)
        .await?;
    let tools: Arc<Vec<Box<dyn ToolDyn>>> = Arc::new(vec![Box::new(
        MailboxCreateTool::new(
            node.clone(),
            MailboxNotificationPolicy {
                kind: MailboxKind::Ask,
                action: MailboxAction::StartRequest,
                ..Default::default()
            },
        )
        .with_access(agent_access.clone()),
    )]);
    let tools = bind_request_tools(tools, context);
    let args = serde_json::json!({"title":"delegated notice"}).to_string();
    let receipt = tools[0].call_with_receipt(args.clone()).await.result?;
    let receipt: serde_json::Value = serde_json::from_str(&receipt)?;
    assert_eq!(
        receipt["outcome"],
        serde_json::to_value(MailboxWriteOutcome::Created)?
    );
    assert_eq!(receipt["item"]["requester_did"], user.did());
    assert_eq!(receipt["item"]["request_id"], "mailbox-delegated-request");
    let doc = receipt["item"]["_docID"].as_str().unwrap();
    let query = format!("{{ MailboxItem(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ _docID _version {{ cid height fieldName }} }} }}", crate::store::graphql::escape_graphql_string(doc));
    let rows = agent_access.execute(&query).await?;
    let version = crate::store::graphql::document_composite_version(
        &rows["data"]["MailboxItem"][0],
        "mailbox item",
    )?
    .unwrap();
    assert_block_signer(&node, &version.cid, agent.did()).await;
    let policy = &crate::lean_vocab_test::lean_contract_snapshot().mailbox_native_policy;
    for (role, access) in [("user", &user_access), ("other", &outsider_access)] {
        let expected = policy["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["actor"] == role && case["permission"] == "read")
            .unwrap()["expected"]
            .as_bool()
            .unwrap();
        let result = access.execute(&query).await?;
        assert_eq!(
            !result["data"]["MailboxItem"].as_array().unwrap().is_empty(),
            expected,
            "{role}"
        );
    }
    assert!(
        crate::session::mailbox::dismiss_mailbox_item_on(&outsider_access, doc)
            .await
            .is_err()
    );
    agent_access
        .delete_actor_relationship("MailboxItem", doc, "recipient", &user.did().parse()?)
        .await?;
    let reused: serde_json::Value =
        serde_json::from_str(&tools[0].call_with_receipt(args).await.result?)?;
    assert_eq!(
        reused["outcome"],
        serde_json::to_value(MailboxWriteOutcome::Reused)?
    );
    assert_eq!(reused["item"]["_docID"], doc);
    assert!(user_access.execute(&query).await?["data"]["MailboxItem"]
        .as_array()
        .unwrap()
        .is_empty());
    agent_access
        .add_actor_relationship("MailboxItem", doc, "recipient", &user.did().parse()?)
        .await?;
    let stream = crate::request::streaming::DefraStreamWriter::new_for_lifecycle(
        node.clone(),
        &lifecycle,
        std::time::Duration::ZERO,
    )?;
    lifecycle.begin_owned_execution(&stream).await?;
    assert_eq!(
        lifecycle
            .terminalize_owned(
                RequestTerminalOutcome::Completed,
                gents_protocol::output::TerminalOutput::NoMessage,
                None
            )
            .await?,
        TerminalizeResult::Won
    );
    owner_access
        .add_actor_relationship("Agent", &agent_doc, "session_writer", &user.did().parse()?)
        .await?;
    let mut reply = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "mailbox-delegated-reply",
        runtime.did(),
        owner.did(),
        agent.did(),
        "mailbox-delegated-session",
        "Approved",
        "interactive",
        "2026-11-04T00:00:00Z",
        AgentRequestAdmissionRecord::client(user.did()),
    );
    reply.session_doc_id = create.session_doc_id.clone();
    reply.agent_doc_id = create.agent_doc_id.clone();
    reply.caused_by_source_doc_id = Some(doc.to_owned());
    crate::sign_agent_request_create(&user, &mut reply).await?;
    submit_client_request(&user_access, &reply).await?;
    let reply_row = agent_access.execute(&format!("{{ AgentRequest(filter: {{request_id: {{_eq: \"mailbox-delegated-reply\"}}}}) {{ {} }} }}", crate::SIGNED_REQUEST_FIELDS)).await?;
    let reply_row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(reply_row["data"]["AgentRequest"][0].clone())?;
    let reply_request = AgentRequest::try_from(reply_row)?;
    let (reply_request, reply_admission) = verifier
        .verify_fresh_for_claim(&reply_request, agent.did())
        .await?;
    let mut reply_lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        agent.did(),
        runtime.did(),
        reply_request,
        60,
        ExecutionOrigin::Interactive,
        String::new(),
    );
    reply_lifecycle.bind_managed_agent(agent.as_ref())?;
    reply_lifecycle.set_client_claim_admission(reply_admission);
    agent_access
        .delete_actor_relationship("MailboxItem", doc, "recipient", &user.did().parse()?)
        .await?;
    assert!(reply_lifecycle.claim_with_identity().await.is_err());
    assert_eq!(
        crate::session::mailbox::load_mailbox_item_with_access(&agent_access, doc)
            .await?
            .unwrap()
            .status,
        "open"
    );
    agent_access
        .add_actor_relationship("MailboxItem", doc, "recipient", &user.did().parse()?)
        .await?;
    assert_eq!(
        reply_lifecycle.claim_with_identity().await?,
        ClaimOutcome::Claimed
    );
    assert_eq!(
        crate::session::mailbox::load_mailbox_item_with_access(&agent_access, doc)
            .await?
            .unwrap()
            .status,
        "acted"
    );
    agent_access
        .delete_actor_relationship("MailboxItem", doc, "recipient", &user.did().parse()?)
        .await?;
    let claimed_reply = reply_lifecycle.request().clone();
    agent_access
        .transact_write("test.mailbox.reply_replay", |txn| {
            let claimed_reply = &claimed_reply;
            Box::pin(async move {
                crate::session::mailbox::claim_reply_in_txn(
                    txn,
                    claimed_reply,
                    &chrono::Utc::now().to_rfc3339(),
                )
                .await
            })
        })
        .await?;
    node.shutdown().await?;
    Ok(())
}
