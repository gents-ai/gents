use super::*;
use crate::request::admission::AgentRequestAdmissionVerifier;
use crate::request::lifecycle::{
    ClaimOutcome, ExecutionOrigin, RequestLifecycle, RequestTerminalOutcome, TerminalizeResult,
};
use crate::session::mailbox::{MailboxCreateTool, MailboxStatus};
use gents_documents::mailbox_write_decl::{
    MailboxAction, MailboxKind, MailboxNotificationPolicy, MailboxWriteOutcome,
};
use gents_loop::tool::{bind_request_tools, ToolDyn};
use gents_protocol::row::AgentRequest;
use gents_store::config_client::NodePermission;

#[tokio::test]
async fn delegated_http_claim_routes_mailbox_to_immediate_signer() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let db = gents_inference::test_support::test_db(
        "delegated-http-claim-routes-mailbox-to",
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = db.node.clone();
    let runtime: Arc<dyn NodeIdentity> = Arc::new(
        gents_store::test_support::held_runtime_identity(db.node_identity.did()),
    );
    let owner = KeyIdentity::load_or_create(home.path().join("owner.key"), None)?;
    let user = KeyIdentity::load_or_create(home.path().join("user.key"), None)?;
    let outsider = KeyIdentity::load_or_create(home.path().join("other.key"), None)?;
    let binding = gents_store::identity::provision_agent_identity(runtime.as_ref())?;
    let agent = gents_store::identity::resolve_agent_identity(runtime.as_ref(), &binding)?;
    let admin = ConfigAccess::local_as(node.clone(), runtime.did().parse()?);
    grant_operator_user(&node, runtime.as_ref(), &owner).await;
    for did in [user.did(), outsider.did(), agent.did()] {
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
    let client = |key: &KeyIdentity| -> anyhow::Result<ConfigAccess> { Ok(db.client_access(key)) };
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
    // The delegate signs the request, so the claim requires its update on the
    // session, as the modeled cross-actor claim does of the immediate signer.
    owner_access
        .add_actor_relationship(
            "AgentSession",
            &session_doc,
            "working_agent",
            &user.did().parse()?,
        )
        .await?;
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
    crate::request::admission::sign_agent_request_create(&user, &mut create).await?;
    submit_client_request(&user_access, &create).await?;
    let agent_access =
        ConfigAccess::local_managed_agent(node.clone(), agent.did().parse()?, agent.as_ref())?;
    let response = agent_access
        .execute(&format!(
            "{{ AgentRequest {{ {} }} }}",
            gents_protocol::request_admission::SIGNED_REQUEST_FIELDS
        ))
        .await?;
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(response["data"]["AgentRequest"][0].clone())?;
    let request = AgentRequest::try_from(row)?;
    let (_authority_owner, authority) = gents_p2p::p2p_reconcile::enrollment_authority_channel();
    let verifier = AgentRequestAdmissionVerifier::new(node.clone(), runtime.clone(), authority);
    let (request, admission) = verifier
        .verify_fresh_for_claim(&request, agent.did())
        .await?;
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        request,
        60,
        ExecutionOrigin::Interactive,
        String::new(),
    );
    lifecycle.bind_managed_agent(agent.as_ref())?;
    lifecycle.set_client_claim_admission(admission);
    let mut before_claim = Some(lifecycle.claimed_tool_context().is_ok());
    assert_eq!(
        lifecycle.claim_with_identity().await?,
        ClaimOutcome::Claimed
    );
    let context = lifecycle.claimed_tool_context()?;
    let mut after_claim = Some(context.admitted_client.is_some());
    for case in gents_lean_vocab::families::mailbox_access_claim_tool_context::cases() {
        case.check(
            match case.input.committed {
                false => before_claim.take(),
                true => after_claim.take(),
            }
            .expect("one tool context observation per modeled claim step"),
        );
    }
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
    let query = format!("{{ MailboxItem(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ _docID _version {{ cid height fieldName }} }} }}", gents_store::graphql::escape_graphql_string(doc));
    let rows = agent_access.execute(&query).await?;
    let version = gents_store::graphql::document_composite_version(
        &rows["data"]["MailboxItem"][0],
        "mailbox item",
    )?
    .unwrap();
    assert_block_signer(&node, &version.cid, agent.did()).await;
    let tool_visible = |access: &ConfigAccess| {
        let (access, query) = (access.clone(), query.clone());
        async move {
            Ok::<_, anyhow::Error>(
                !access.execute(&query).await?["data"]["MailboxItem"]
                    .as_array()
                    .unwrap()
                    .is_empty(),
            )
        }
    };
    let mut tool_user = Some(tool_visible(&user_access).await?);
    let mut tool_other = Some(tool_visible(&outsider_access).await?);
    assert!(crate::operations::mailbox::dismiss(&outsider_access, doc)
        .await
        .is_err());
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
    let runtime_notice = |request_id: &str| crate::session::mailbox::RuntimeNotice {
        source_id: format!("service-update:{request_id}"),
        session_id: "mailbox-delegated-session".into(),
        request_id: request_id.into(),
        title: "Runtime update complete".into(),
        summary: "Completed".into(),
        failed: false,
    };
    // After the claimed request is terminal, as after a runtime restart, the
    // notice reaches the verified signer rather than the session owner.
    let notice = crate::session::mailbox::publish_runtime_notice(
        &node,
        runtime.did(),
        std::slice::from_ref(&agent),
        runtime_notice("mailbox-delegated-request"),
    )
    .await;
    let claimed_document = notice.is_ok();
    let claimed_deferred = matches!(
        notice,
        Err(crate::session::mailbox::RuntimeNoticeError::MissingClaimProvenance)
    );
    let notice = notice?;
    assert_eq!(notice.requester_did, user.did());
    assert_eq!(notice.target_agent_did, agent.did());
    let notice_query = format!(
        "{{ MailboxItem(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ _docID }} }}",
        gents_store::graphql::escape_graphql_string(&notice.doc_id)
    );
    let visible = |access: &ConfigAccess, query: String| {
        let access = access.clone();
        async move {
            Ok::<_, anyhow::Error>(
                !access.execute(&query).await?["data"]["MailboxItem"]
                    .as_array()
                    .unwrap()
                    .is_empty(),
            )
        }
    };
    let mut notice_other = Some(visible(&owner_access, notice_query.clone()).await?);
    for case in gents_lean_vocab::families::mailbox_access_claim_visibility::cases() {
        case.check(
            match (case.input.item.as_str(), case.input.actor.as_str()) {
                ("tool", "user") => tool_user.take(),
                ("tool", "other") => tool_other.take(),
                ("notice", "other") => notice_other.take(),
                _ => None,
            }
            .unwrap_or_else(|| panic!("no served observation for {:?}", case.input)),
        );
    }
    let mut claimed_notice = Some(
        gents_lean_vocab::families::mailbox_access_notice_publication::NoticeOutput {
            document: claimed_document,
            recipient: visible(&user_access, notice_query.clone()).await?,
            deferred: claimed_deferred,
        },
    );
    crate::operations::mailbox::dismiss(&user_access, &notice.doc_id).await?;
    let replayed = crate::session::mailbox::publish_runtime_notice(
        &node,
        runtime.did(),
        std::slice::from_ref(&agent),
        runtime_notice("mailbox-delegated-request"),
    )
    .await?;
    assert_eq!(replayed.doc_id, notice.doc_id);
    assert_eq!(replayed.parsed_status(), Some(MailboxStatus::Dismissed));
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
    crate::request::admission::sign_agent_request_create(&user, &mut reply).await?;
    submit_client_request(&user_access, &reply).await?;
    let unclaimed_notice = |request_id: &'static str| {
        let (node, agent, agent_access, user_access) = (
            node.clone(),
            agent.clone(),
            agent_access.clone(),
            user_access.clone(),
        );
        let runtime_did = runtime.did().to_owned();
        async move {
            let result = crate::session::mailbox::publish_runtime_notice(
                &node,
                &runtime_did,
                std::slice::from_ref(&agent),
                runtime_notice(request_id),
            )
            .await;
            let deferred = matches!(
                result,
                Err(crate::session::mailbox::RuntimeNoticeError::MissingClaimProvenance)
            );
            let source = format!("service-update:{request_id}");
            let filter = format!(
                "{{ MailboxItem(filter: {{source_id: {{_eq: \"{}\"}}}}) {{ _docID }} }}",
                gents_store::graphql::escape_graphql_string(&source)
            );
            Ok::<_, anyhow::Error>(
                gents_lean_vocab::families::mailbox_access_notice_publication::NoticeOutput {
                    document: result.is_ok()
                        || !agent_access.execute(&filter).await?["data"]["MailboxItem"]
                            .as_array()
                            .unwrap()
                            .is_empty(),
                    recipient: !user_access.execute(&filter).await?["data"]["MailboxItem"]
                        .as_array()
                        .unwrap()
                        .is_empty(),
                    deferred,
                },
            )
        }
    };
    // Admitted and visible, but its claim has not committed.
    let mut unclaimed = Some(unclaimed_notice("mailbox-delegated-reply").await?);
    let mut missing = Some(unclaimed_notice("mailbox-missing-request").await?);
    for case in gents_lean_vocab::families::mailbox_access_notice_publication::cases() {
        case.check(
            match (case.input.present, case.input.claimed) {
                (true, true) => claimed_notice.take(),
                (true, false) => unclaimed.take(),
                (false, _) => missing.take(),
            }
            .expect("one served observation per modeled notice"),
        );
    }
    let reply_row = agent_access.execute(&format!("{{ AgentRequest(filter: {{request_id: {{_eq: \"mailbox-delegated-reply\"}}}}) {{ {} }} }}", gents_protocol::request_admission::SIGNED_REQUEST_FIELDS)).await?;
    let reply_row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(reply_row["data"]["AgentRequest"][0].clone())?;
    let reply_request = AgentRequest::try_from(reply_row)?;
    let (reply_request, reply_admission) = verifier
        .verify_fresh_for_claim(&reply_request, agent.did())
        .await?;
    let mut reply_lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
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
        crate::operations::mailbox::get(&agent_access, doc)
            .await?
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
        crate::operations::mailbox::get(&agent_access, doc)
            .await?
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
