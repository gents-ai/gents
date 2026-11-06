use std::sync::Arc;

use defra_node::Node as EmbeddedNode;

use super::*;
use crate::store::identity::NodeIdentity;

const FIXTURE_CLOSE: &[MailboxCloseCollection] = &[MailboxCloseCollection {
    collection: "MailboxFixture",
    correlation_field: "mailbox_item_key",
}];

pub(super) fn root_identity() -> crate::store::identity::KeyIdentity {
    let home = tempfile::tempdir().unwrap();
    crate::request::lifecycle::test_support::pin_fixed_signing_identity(home.path())
}

pub(super) fn agent_identity() -> Arc<dyn NodeIdentity> {
    let root = root_identity();
    let nonce = "b".repeat(64);
    let binding = gents_protocol::agent_identity::AgentIdentityBinding {
        agent_did: crate::store::identity::derive_agent_identity(&root, &nonce)
            .unwrap()
            .did()
            .to_owned(),
        creation_nonce: nonce,
    };
    crate::store::identity::resolve_agent_identity(&root, &binding).unwrap()
}

pub(super) fn agent_access(node: &Arc<EmbeddedNode>) -> crate::ConfigAccess {
    let agent = agent_identity();
    crate::ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap()
}

pub(super) fn fixture_recipient(label: &str) -> String {
    if !label.starts_with("did:test:") {
        return label.to_owned();
    }
    static RECIPIENTS: std::sync::OnceLock<Vec<(&'static str, crate::KeyIdentity)>> =
        std::sync::OnceLock::new();
    let recipients = RECIPIENTS.get_or_init(|| {
        let directory = tempfile::tempdir().unwrap();
        [
            "did:test:owner",
            "did:test:owner-a",
            "did:test:owner-b",
            "did:test:delegate",
            "did:test:alice",
            "did:test:bob",
        ]
        .into_iter()
        .enumerate()
        .map(|(index, label)| {
            (
                label,
                crate::KeyIdentity::load_or_create(
                    directory.path().join(format!("client-{index}.key")),
                    None,
                )
                .unwrap(),
            )
        })
        .collect()
    });
    recipients
        .iter()
        .find(|(name, _)| *name == label)
        .expect("known mailbox client fixture")
        .1
        .did()
        .to_owned()
}

pub(super) async fn load_as_creator(
    node: &Arc<EmbeddedNode>,
    doc_id: &str,
) -> Result<Option<MailboxItem>> {
    load_mailbox_item_with_access(&agent_access(node), doc_id).await
}

pub(super) async fn list_as_creator(
    node: &Arc<EmbeddedNode>,
    requester: &str,
    status: Option<MailboxStatus>,
) -> Result<Vec<MailboxItem>> {
    list_mailbox_items_on(&agent_access(node), &fixture_recipient(requester), status).await
}

pub(super) async fn dismiss_as_creator(
    node: &Arc<EmbeddedNode>,
    doc_id: &str,
) -> Result<MailboxItem> {
    dismiss_mailbox_item_on(&agent_access(node), doc_id).await
}

pub(super) async fn stamp_create(
    node: &Arc<EmbeddedNode>,
    context: &MailboxStampContext,
    args: FileMailboxItemArgs,
) -> Result<MailboxItem> {
    stamp_create_with_access(
        node,
        &agent_access(node),
        context,
        args,
        MAILBOX_CLOSE_COLLECTIONS,
    )
    .await
}

pub(super) async fn stamp_create_with_close_collections(
    node: &Arc<EmbeddedNode>,
    context: &MailboxStampContext,
    args: FileMailboxItemArgs,
    collections: &[MailboxCloseCollection],
) -> Result<MailboxItem> {
    anyhow::ensure!(
        context.node_did == root_identity().did(),
        "foreign-node mailbox observations require replication from their owning node"
    );
    stamp_create_with_access(node, &agent_access(node), context, args, collections).await
}

/// A foreign node label exercises the page predicate; transport provenance is
/// covered by the separate exact-peer-pull regression.
async fn seed_foreign_mailbox_row(
    node: &Arc<EmbeddedNode>,
    source: &str,
    deadline: &str,
) -> String {
    let source = escape_graphql_string(source);
    let deadline = escape_graphql_string(deadline);
    let agent = escape_graphql_string(agent_identity().did());
    let now = Utc::now().to_rfc3339();
    let response = agent_access(node).write("test.mailbox.foreign_label", &format!(r#"mutation {{ create_MailboxItem(input: {{
        item_key: "session:{source}:ask:1", requester_did: "did:test:owner", node_did: "did:test:foreign",
        status: "open", kind: "ask", action: "write_document", title: "fixture", source_kind: "session",
        source_id: "{source}", target_node_did: "did:test:foreign", target_agent_did: "{agent}",
        expected_collection: "MailboxFixture", deadline_at: "{deadline}", created_at: "{now}", updated_at: "{now}"
    }}) {{ _docID }} }}"#)).await.unwrap();
    crate::store::graphql::created_doc_id(&response, MAILBOX_COLLECTION).unwrap()
}

pub(super) async fn test_node() -> Arc<EmbeddedNode> {
    let root = root_identity();
    let node = Arc::new(
        EmbeddedNode::builder()
            .enable_signing()
            .with_node_identity_did(root.did())
            .with_node_acp_enabled()
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let root_access =
        crate::ConfigAccess::local_as(node.clone(), identity::Did::new(root.did()).unwrap());
    let agent = identity::Did::new(agent_identity().did()).unwrap();
    for permission in [
        crate::store::config_client::NodePermission::DocumentRead,
        crate::store::config_client::NodePermission::DocumentUpdate,
        crate::store::config_client::NodePermission::CollectionGet,
        crate::store::config_client::NodePermission::DacRelationAdd,
    ] {
        root_access
            .grant_node_permission(&agent, permission)
            .await
            .unwrap();
    }
    node.add_collection(
        r#"
        type MailboxFixture {
            mailbox_item_key: String @index(unique: true) @immutable
            body: String
        }
        "#,
    )
    .await
    .unwrap();
    node
}

pub(super) fn context(owner: &str) -> MailboxStampContext {
    MailboxStampContext {
        requester_did: fixture_recipient(owner),
        node_did: root_identity().did().into(),
        agent_did: agent_identity().did().into(),
        session_id: Some("session-1".into()),
    }
}

pub(super) fn args(action: MailboxAction, source_id: &str) -> FileMailboxItemArgs {
    FileMailboxItemArgs {
        kind: MailboxKind::Ask,
        action,
        title: "Review the result".into(),
        summary: Some("A durable attention envelope".into()),
        payload: None,
        source_kind: MailboxSourceKind::Session,
        source_id: source_id.into(),
        session_id: None,
        request_id: None,
        cause_doc_id: None,
        expected_collection: (action == MailboxAction::WriteDocument)
            .then(|| "MailboxFixture".into()),
        parent_item_id: None,
        deadline_at: None,
    }
}

#[tokio::test]
async fn stamping_is_owner_scoped_idempotent_and_never_reopens() {
    let node = test_node().await;
    let first = stamp_create(
        &node,
        &context("did:test:owner-a"),
        args(MailboxAction::Ack, "wait-1"),
    )
    .await
    .unwrap();
    let retry = stamp_create(
        &node,
        &context("did:test:owner-a"),
        args(MailboxAction::Ack, "wait-1"),
    )
    .await
    .unwrap();
    assert_eq!(first.doc_id, retry.doc_id);
    assert_eq!(first.item_key, "session:wait-1:ask:1");

    let collision = stamp_create(
        &node,
        &context("did:test:owner-b"),
        args(MailboxAction::Ack, "wait-1"),
    )
    .await
    .unwrap_err();
    assert!(collision.to_string().contains("collision"));

    assert!(dismiss_mailbox_item(&node, &first.doc_id, "did:test:other")
        .await
        .is_err());
    dismiss_as_creator(&node, &first.doc_id).await.unwrap();
    let second = stamp_create(
        &node,
        &context("did:test:owner-a"),
        args(MailboxAction::Ack, "wait-1"),
    )
    .await
    .unwrap();
    assert_eq!(second.item_key, "session:wait-1:ask:2");
    assert_ne!(second.doc_id, first.doc_id);

    let duplicate_open = format!(
        r#"mutation {{ create_MailboxItem(input: {{
            item_key: "session:wait-1:ask:3", requester_did: "did:test:owner-a",
            node_did: "did:test:agent", status: "open", kind: "ask", action: "ack",
            title: "duplicate", summary: null, payload: null, source_kind: "session",
            source_id: "wait-1", session_id: null, request_id: null,
            cause_doc_id: null, target_node_did: "did:test:agent",
            target_agent_did: "operator", expected_collection: null, parent_item_id: null,
            deadline_at: null, created_at: "{}", updated_at: "{}",
            resolved_at: null, resolved_doc_id: null
        }}) {{ _docID }} }}"#,
        Utc::now().to_rfc3339(),
        Utc::now().to_rfc3339()
    );
    let duplicate_open = duplicate_open
        .replace("did:test:owner-a", &fixture_recipient("did:test:owner-a"))
        .replace("did:test:agent", root_identity().did())
        .replace("operator", agent_identity().did());
    agent_access(&node)
        .write("test.mailbox.duplicate", &duplicate_open)
        .await
        .unwrap();
    let error = stamp_create(
        &node,
        &context("did:test:owner-a"),
        args(MailboxAction::Ack, "wait-1"),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("more than one"));
}

#[tokio::test]
async fn stamping_rejects_missing_context_unknown_fields_and_unsupported_domain_routes() {
    let node = test_node().await;
    let mut missing = context("");
    missing.requester_did.clear();
    assert!(
        stamp_create(&node, &missing, args(MailboxAction::Ack, "wait-2"))
            .await
            .is_err()
    );
    assert!(serde_json::from_value::<FileMailboxItemArgs>(json!({
        "kind": "ask", "action": "ack", "title": "x", "source_kind": "session",
        "source_id": "wait-2", "requester_did": "did:test:forged"
    }))
    .is_err());
    let mut unsupported = args(MailboxAction::WriteDocument, "wait-3");
    unsupported.expected_collection = Some("MissingMailboxResponse".into());
    assert!(
        stamp_create(&node, &context("did:test:owner"), unsupported,)
            .await
            .is_err()
    );
    assert!(validate_close_collections(&[MailboxCloseCollection {
        collection: "Bad) { hacked",
        correlation_field: "mailbox_item_key",
    }])
    .is_err());
}

#[tokio::test]
async fn event_stage_materialization_persists_explicit_requester_lineage() {
    let key_path =
        std::env::temp_dir().join(format!("mailbox-event-stage-{}.key", uuid::Uuid::new_v4()));
    let identity = crate::store::identity::KeyIdentity::load_or_create(key_path, None).unwrap();
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(identity.did())
            .with_node_acp_owner(identity.did().to_owned())
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let trigger_context = serde_json::json!({
        "version": 1,
        "source_fields": {"requester_did": identity.did()}
    })
    .to_string();
    let parsed =
        crate::request::lifecycle::TriggerExecutionContext::parse(Some(&trigger_context)).unwrap();
    let requester_did = parsed
        .source_fields
        .get("requester_did")
        .map(String::as_str);
    let binding = crate::store::identity::provision_agent_identity(&identity).unwrap();
    let agent = crate::store::identity::resolve_agent_identity(&identity, &binding).unwrap();
    let root_access =
        crate::ConfigAccess::local_as(node.clone(), identity::Did::new(identity.did()).unwrap());
    for permission in [
        crate::store::config_client::NodePermission::DocumentRead,
        crate::store::config_client::NodePermission::DocumentUpdate,
        crate::store::config_client::NodePermission::DacRelationAdd,
    ] {
        root_access
            .grant_node_permission(&identity::Did::new(agent.did()).unwrap(), permission)
            .await
            .unwrap();
    }
    let access = crate::ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    let mut stamp_context = context(identity.did());
    stamp_context.node_did = identity.did().into();
    stamp_context.agent_did = agent.did().into();
    let source_item = stamp_create_with_access(
        &node,
        &access,
        &stamp_context,
        args(MailboxAction::StartRequest, "event-observed-mailbox"),
        MAILBOX_CLOSE_COLLECTIONS,
    )
    .await
    .unwrap();
    let enqueued = crate::request::lifecycle::materialize::write_pending_agent_request_with_lineage_workspace_and_conversation_title(
        &node,
        ::identity::Did::new(agent.did().to_owned()).expect("fixture managed Agent DID"),
        identity.did(),
        agent.did(),
        "continue the run",
        crate::request::lifecycle::ExecutionOrigin::Scheduled,
        crate::request::lifecycle::TriggerLineage {
            trigger_id: Some("event-trigger-1".into()),
            trigger_kind: Some("event".into()),
            source_doc_id: Some(source_item.doc_id.clone()),
            correlation: Some("run-1".into()),
            trigger_context: Some(trigger_context),
        },
        None,
        None,
        None,
        requester_did,
        Some("event-trigger-config-doc-1"),
        None,
        agent.as_ref(),
    )
    .await
    .unwrap();
    let response = graphql_with_transaction_retry(
        node.as_ref(),
        &format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ requester_did caused_by_source_doc_id }} }}"#,
            escape_graphql_string(&enqueued.doc_id)
        ),
        "verify event-stage owner lineage",
    )
    .await
    .unwrap();
    let rows = rows::<Value>(&response, "AgentRequest").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["requester_did"], identity.did());
    assert_eq!(rows[0]["caused_by_source_doc_id"], source_item.doc_id);
    let response = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
            escape_graphql_string(&enqueued.doc_id),
            crate::request::admission::SIGNED_REQUEST_FIELDS,
        ))
        .await;
    let request = crate::request::watcher::AgentRequest::try_from(
        crate::store::graphql::rows::<gents_protocol::row::AgentRequestRow>(
            &response,
            "AgentRequest",
        )
        .unwrap()
        .remove(0),
    )
    .unwrap();
    let request = &request;
    crate::store::config_client::ConfigAccess::transact_local_write(
        &node,
        None,
        "test.mailbox_event",
        |txn| {
            Box::pin(
                async move { claim_reply_in_txn(txn, request, &Utc::now().to_rfc3339()).await },
            )
        },
    )
    .await
    .unwrap();
    assert_eq!(
        load_as_creator(&node, &source_item.doc_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "open"
    );
}

#[tokio::test]
async fn close_engines_are_disjoint_and_unsigned_replies_do_not_prevent_expiry() {
    let node = test_node().await;
    let observed_item = stamp_create(
        &node,
        &context("did:test:owner"),
        args(MailboxAction::StartRequest, "wait-observed"),
    )
    .await
    .unwrap();
    let event_child = format!(
        r#"mutation {{ create_AgentRequest(input: {{
            request_id: "mailbox-observer-child", purpose: "normal", node_did: "did:test:agent",
            requester_did: "did:test:owner", agent_did: "operator",
            session_id: "session-1", content: "observe",
            lifecycle_state: "pending", execution_origin: "scheduled",
            caused_by_trigger_kind: "event", caused_by_source_doc_id: "{}",
            created_at: "{}"
        }}) {{ _docID }} }}"#,
        escape_graphql_string(&observed_item.doc_id),
        Utc::now().to_rfc3339()
    );
    let event_child = event_child
        .replace("did:test:owner", &fixture_recipient("did:test:owner"))
        .replace("did:test:agent", root_identity().did())
        .replace("operator", agent_identity().did());
    agent_access(&node)
        .write("test.mailbox.event_child", &event_child)
        .await
        .unwrap();
    let observer_report = sweep_open_mailbox_items_with_close_collections(
        &node,
        root_identity().did(),
        &[agent_identity()],
        None,
        FIXTURE_CLOSE,
    )
    .await
    .unwrap()
    .report;
    assert_eq!(observer_report.acted, 0);
    assert_eq!(
        load_as_creator(&node, &observed_item.doc_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "open"
    );

    let past = (Utc::now() - chrono::Duration::minutes(1)).to_rfc3339();
    let mut request_args = args(MailboxAction::StartRequest, "wait-request");
    request_args.deadline_at = Some(past.clone());
    let request_item = stamp_create(&node, &context("did:test:owner"), request_args)
        .await
        .unwrap();
    let create_request = format!(
        r#"mutation {{ create_AgentRequest(input: {{
            request_id: "mailbox-request", purpose: "normal", node_did: "did:test:agent",
            requester_did: "did:test:owner", agent_did: "operator",
            session_id: "session-1", content: "continue",
            lifecycle_state: "pending", execution_origin: "interactive",
            caused_by_source_doc_id: "{}", created_at: "{}"
        }}) {{ _docID }} }}"#,
        escape_graphql_string(&request_item.doc_id),
        Utc::now().to_rfc3339()
    );
    let create_request = create_request
        .replace("did:test:owner", &fixture_recipient("did:test:owner"))
        .replace("did:test:agent", root_identity().did())
        .replace("operator", agent_identity().did());
    agent_access(&node)
        .write("test.mailbox.unsigned_reply", &create_request)
        .await
        .unwrap();
    let report = sweep_open_mailbox_items_with_close_collections(
        &node,
        root_identity().did(),
        &[agent_identity()],
        None,
        FIXTURE_CLOSE,
    )
    .await
    .unwrap()
    .report;
    assert_eq!(report.acted, 0);
    assert_eq!(report.expired, 1);

    let domain_args = args(MailboxAction::WriteDocument, "wait-domain");
    let domain_item = stamp_create_with_close_collections(
        &node,
        &context("did:test:owner"),
        domain_args,
        FIXTURE_CLOSE,
    )
    .await
    .unwrap();
    let unrelated_request = format!(
        r#"mutation {{ create_AgentRequest(input: {{ request_id: "transport-only",
            purpose: "normal",
            node_did: "did:test:agent", requester_did: "did:test:owner",
            agent_did: "operator", session_id: "session-1", content: "draft",
            caused_by_source_doc_id: "{}", created_at: "{}" }}) {{ _docID }} }}"#,
        escape_graphql_string(&domain_item.doc_id),
        Utc::now().to_rfc3339()
    );
    let unrelated_request = unrelated_request
        .replace("did:test:owner", &fixture_recipient("did:test:owner"))
        .replace("did:test:agent", root_identity().did())
        .replace("operator", agent_identity().did());
    agent_access(&node)
        .write("test.mailbox.unrelated_request", &unrelated_request)
        .await
        .unwrap();
    let request_only = sweep_open_mailbox_items_with_close_collections(
        &node,
        root_identity().did(),
        &[agent_identity()],
        None,
        FIXTURE_CLOSE,
    )
    .await
    .unwrap()
    .report;
    assert_eq!(request_only.acted, 0);
    assert_eq!(request_only.expired, 0);
    assert_eq!(
        load_as_creator(&node, &domain_item.doc_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "open"
    );
    let fixture = format!(
        r#"mutation {{ create_MailboxFixture(input: {{ mailbox_item_key: "{}", body: "done" }}) {{ _docID }} }}"#,
        escape_graphql_string(&domain_item.item_key)
    );
    assert!(!node.execute(&fixture).await.has_errors());
    let report = sweep_open_mailbox_items_with_close_collections(
        &node,
        root_identity().did(),
        &[agent_identity()],
        None,
        FIXTURE_CLOSE,
    )
    .await
    .unwrap()
    .report;
    assert_eq!(report.acted, 1);
    assert_eq!(report.expired, 0);
    let closed = load_as_creator(&node, &domain_item.doc_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(closed.status, "acted");
    assert!(closed.resolved_doc_id.is_some());
}

#[tokio::test]
async fn a_0_21_graph_sourced_item_still_lists() {
    let node = test_node().await;
    let now = Utc::now().to_rfc3339();
    let stored = format!(
        r#"mutation {{ create_MailboxItem(input: {{
            item_key: "graph:wait-1:ask:1", requester_did: "did:test:owner",
            node_did: "did:test:agent", status: "open", kind: "ask", action: "ack",
            title: "from a graph run", source_kind: "graph", source_id: "wait-1",
            graph_run_id: "run-1", target_node_did: "did:test:agent",
            target_agent_did: "operator", created_at: "{now}", updated_at: "{now}"
        }}) {{ _docID }} }}"#
    );
    let stored = stored
        .replace("did:test:owner", &fixture_recipient("did:test:owner"))
        .replace("did:test:agent", root_identity().did())
        .replace("operator", agent_identity().did());
    agent_access(&node)
        .write("test.mailbox.graph", &stored)
        .await
        .unwrap();

    let items = list_as_creator(&node, "did:test:owner", None)
        .await
        .unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].source_kind, "graph");
}

#[tokio::test]
async fn poisoned_item_does_not_block_later_expiry() {
    let node = test_node().await;
    let now = Utc::now().to_rfc3339();
    let hostile = format!(
        r#"mutation {{ create_MailboxItem(input: {{
            item_key: "session:hostile:gate:1", requester_did: "did:test:owner",
            node_did: "did:test:agent", status: "open", kind: "gate",
            action: "write_document", title: "hostile", source_kind: "session",
            source_id: "hostile", target_node_did: "did:test:agent",
            target_agent_did: "operator", created_at: "{now}", updated_at: "{now}"
        }}) {{ _docID }} }}"#
    );
    let hostile = hostile
        .replace("did:test:agent", root_identity().did())
        .replace("operator", agent_identity().did());
    agent_access(&node)
        .write("test.mailbox.hostile", &hostile)
        .await
        .unwrap();
    let undecodable = format!(
        r#"mutation {{ create_MailboxItem(input: {{
            item_key: "session:undecodable:gate:1", requester_did: "did:test:owner",
            node_did: "did:test:agent", status: "open", kind: "gate",
            action: "ack", source_kind: "session", source_id: "undecodable",
            target_node_did: "did:test:agent", target_agent_did: "operator",
            created_at: "{now}", updated_at: "{now}"
        }}) {{ _docID }} }}"#
    );
    let undecodable = undecodable
        .replace("did:test:agent", root_identity().did())
        .replace("operator", agent_identity().did());
    agent_access(&node)
        .write("test.mailbox.undecodable", &undecodable)
        .await
        .unwrap();

    let mut expiring_args = args(MailboxAction::Ack, "wait-expiry");
    expiring_args.deadline_at = Some((Utc::now() - chrono::Duration::minutes(1)).to_rfc3339());
    let expiring = stamp_create(&node, &context("did:test:owner"), expiring_args)
        .await
        .unwrap();

    let report = sweep_open_mailbox_items_with_close_collections(
        &node,
        root_identity().did(),
        &[agent_identity()],
        None,
        FIXTURE_CLOSE,
    )
    .await
    .unwrap()
    .report;
    assert_eq!(report.skipped_errors, 2);
    assert_eq!(report.expired, 1);
    assert_eq!(
        load_as_creator(&node, &expiring.doc_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "expired"
    );
}

#[tokio::test]
async fn mailbox_sweep_pages_by_node_and_progresses_while_rows_close() {
    use std::collections::BTreeMap;

    let node = test_node().await;
    let contract = crate::lean_vocab_test::lean_contract_snapshot();
    let case = contract
        .mailbox_sweep_cases
        .first()
        .expect("Lean mailbox sweep case");
    let node_did = case["node_did"].as_str().unwrap();
    assert_eq!(
        case["page_size"].as_u64(),
        Some(MAILBOX_SWEEP_PAGE_SIZE as u64)
    );
    assert_eq!(
        case["budget"].as_u64(),
        Some(MAILBOX_SWEEP_PAGE_SIZE as u64)
    );
    let actions = case["actions"].as_array().unwrap();
    assert_eq!(actions.len(), 4);
    let future = (Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    let mut modeled_doc_ids = BTreeMap::<u64, String>::new();
    let mut modeled_item_keys = BTreeMap::<u64, String>::new();
    let mut modeled_ids_by_doc = BTreeMap::<String, u64>::new();
    let mut modeled_ids_by_item_key = BTreeMap::<String, u64>::new();
    let mut modeled_due = BTreeMap::<u64, bool>::new();
    let mut created_local_ids = Vec::new();
    let mut foreign_doc_id = None;
    for row in actions[0]["rows"].as_array().unwrap() {
        let model_id = row["doc_id"].as_u64().unwrap();
        let due = row["due"].as_bool().unwrap();
        modeled_due.insert(model_id, due);
        let local = row["node_did"].as_str().unwrap() == node_did;
        let row_context = context("did:test:owner");
        let mut row_args = args(
            MailboxAction::WriteDocument,
            &format!("model-row-{model_id:04}"),
        );
        row_args.deadline_at = Some(future.clone());
        if local {
            let created =
                stamp_create_with_close_collections(&node, &row_context, row_args, FIXTURE_CLOSE)
                    .await
                    .unwrap();
            modeled_doc_ids.insert(model_id, created.doc_id.clone());
            modeled_item_keys.insert(model_id, created.item_key.clone());
            modeled_ids_by_doc.insert(created.doc_id.clone(), model_id);
            modeled_ids_by_item_key.insert(created.item_key, model_id);
            created_local_ids.push(created.doc_id);
        } else {
            foreign_doc_id =
                Some(seed_foreign_mailbox_row(&node, &row_args.source_id, &future).await);
        }
    }

    for index in 0..64 {
        let closed_context = context("did:test:owner");
        let mut closed_args = args(
            MailboxAction::WriteDocument,
            &format!("000-closed-{index:04}"),
        );
        closed_args.deadline_at = Some(future.clone());
        let closed =
            stamp_create_with_close_collections(&node, &closed_context, closed_args, FIXTURE_CLOSE)
                .await
                .unwrap();
        assert!(
            transition_open_item(&agent_access(&node), &closed, MailboxStatus::Acted, None)
                .await
                .unwrap()
        );

        seed_foreign_mailbox_row(&node, &format!("000-foreign-{index:04}"), &future).await;
    }
    let response = agent_access(&node)
        .execute(&format!(
            r#"{{ MailboxItem(filter: {{ _and: [
                {{ node_did: {{ _eq: "{}" }} }},
                {{ status: {{ _eq: "open" }} }}
            ] }}, order: {{ item_key: ASC }}) {{ _docID item_key }} }}"#,
            escape_graphql_string(root_identity().did())
        ))
        .await
        .unwrap();
    let ordered_rows = response["data"][MAILBOX_COLLECTION]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row["_docID"].as_str().unwrap().to_owned(),
                row["item_key"].as_str().unwrap().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(ordered_rows.len(), created_local_ids.len());
    for (doc_id, item_key) in ordered_rows {
        let model_id = modeled_ids_by_doc[&doc_id];
        assert_eq!(modeled_item_keys[&model_id], item_key);
        if modeled_due[&model_id] {
            let item = load_as_creator(&node, &doc_id).await.unwrap().unwrap();
            let response = node.execute(&format!(
                r#"mutation {{ create_MailboxFixture(input: {{ mailbox_item_key: "{}", body: "done" }}) {{ _docID }} }}"#,
                escape_graphql_string(&item.item_key),
            )).await;
            assert!(
                !response.has_errors(),
                "mailbox close fixture: {:?}",
                response.errors
            );
        }
    }
    let mut cursor = None::<String>;
    fn metric(value: &Value, name: &str) -> Option<u64> {
        match value {
            Value::Object(object) => object
                .get(name)
                .and_then(Value::as_u64)
                .or_else(|| object.values().find_map(|value| metric(value, name))),
            Value::Array(array) => array.iter().find_map(|value| metric(value, name)),
            _ => None,
        }
    }
    fn cursor_model_id(token: &str, modeled_ids_by_item_key: &BTreeMap<String, u64>) -> u64 {
        let decoded = cursor::Cursor::decode(token).expect("native cursor token");
        let item_key = decoded.keys["item_key"].as_str().unwrap();
        modeled_ids_by_item_key[item_key]
    }
    fn cursor_doc_id(token: &str) -> String {
        cursor::Cursor::decode(token)
            .expect("native cursor token")
            .doc_id
    }
    for (index, action) in actions.iter().enumerate() {
        let input_cursor = action["cursor"].as_u64();
        assert_eq!(
            cursor
                .as_deref()
                .map(|token| cursor_model_id(token, &modeled_ids_by_item_key)),
            input_cursor,
            "native indexed cursor follows Lean row ordering"
        );
        if let (Some(token), Some(model_id)) = (cursor.as_deref(), input_cursor) {
            assert_eq!(
                cursor_doc_id(token),
                modeled_doc_ids[&model_id],
                "opaque cursor retains the physical identity of the modeled row"
            );
        }
        if index == 1 {
            let current_cursor = cursor.as_deref().expect("second page keyset cursor");
            let cursor_model_id = cursor_model_id(current_cursor, &modeled_ids_by_item_key);
            for model_id in [4, cursor_model_id + 4] {
                assert!(!modeled_due[&model_id]);
                assert_eq!(
                    load_as_creator(&node, &modeled_doc_ids[&model_id])
                        .await
                        .unwrap()
                        .unwrap()
                        .status,
                    "open",
                    "residual open row {model_id} should remain on either side of the cursor"
                );
            }
            let page_query = open_mailbox_page_query(
                root_identity().did(),
                agent_identity().did(),
                Some(current_cursor),
            );
            let planned = agent_access(&node)
                .execute(&format!("query @explain(type: simple) {page_query}"))
                .await
                .unwrap();
            let plan = planned["data"].get("explain").unwrap();
            assert!(
                plan.to_string()
                    .contains("MailboxItem_node_status_item_key"),
                "mailbox page must select its registered compound index: {plan}"
            );
            let explain = agent_access(&node)
                .execute(&format!("query @explain(type: execute) {page_query}"))
                .await
                .unwrap();
            let execution = explain["data"].get("explain").unwrap();
            assert_eq!(execution["executionSuccess"], true, "{execution}");
            let roots = execution["operationNode"].as_array().unwrap();
            assert_eq!(roots.len(), 1);
            let doc_fetches = metric(&roots[0], "docFetches").expect("docFetches metric");
            let index_fetches = metric(&roots[0], "indexFetches").expect("indexFetches metric");
            let remaining_local_open_rows = action["rows"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|row| {
                    row["node_did"] == node_did
                        && row["open"] == true
                        && row["doc_id"]
                            .as_u64()
                            .is_some_and(|id| id > cursor_model_id)
                })
                .count() as u64;
            assert!(
                doc_fetches <= (MAILBOX_SWEEP_PAGE_SIZE + 1) as u64,
                "keyset page must bound document reads, got {doc_fetches}: {execution}"
            );
            assert!(
                index_fetches <= remaining_local_open_rows,
                "keyset index scan should stay within the modeled local open suffix of {remaining_local_open_rows}, got {index_fetches}: {execution}"
            );
        }
        let turn_started = Instant::now();
        let page = sweep_open_mailbox_items_with_clock(
            &node,
            root_identity().did(),
            &[agent_identity()],
            cursor.as_deref(),
            FIXTURE_CLOSE,
            || turn_started,
        )
        .await
        .unwrap();
        assert!(page.report.scanned <= MAILBOX_SWEEP_PAGE_SIZE);
        let selected = action["selected"].as_array().unwrap();
        assert_eq!(page.report.scanned, selected.len());
        let due_selected = selected
            .iter()
            .filter(|model_id| modeled_due[&model_id.as_u64().unwrap()])
            .count();
        assert_eq!(page.report.acted, due_selected);
        assert_eq!(page.report.expired, 0);
        let expected_next = action["next_cursor"].as_u64();
        if let Some(next) = expected_next {
            assert_eq!(
                page.next_cursor
                    .as_deref()
                    .map(|token| { cursor_model_id(token, &modeled_ids_by_item_key) }),
                Some(next),
                "returned cursor is the last processed indexed row"
            );
            assert_eq!(
                page.next_cursor.as_deref().map(cursor_doc_id),
                Some(modeled_doc_ids[&next].clone()),
                "returned cursor retains the corresponding physical document ID"
            );
        } else {
            assert!(page.next_cursor.is_none());
        }
        cursor = page.next_cursor;
    }
    for (model_id, doc_id) in &modeled_doc_ids {
        let expected_status = if modeled_due[model_id] {
            "acted"
        } else {
            "open"
        };
        assert_eq!(
            load_as_creator(&node, doc_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            expected_status
        );
    }
    assert_eq!(
        load_as_creator(&node, foreign_doc_id.as_deref().unwrap())
            .await
            .unwrap()
            .unwrap()
            .status,
        "open"
    );
}

/// MailboxItem carries a collection ACP policy, so the block must be read as
/// the actor that owns the row; the node's own caller cannot read it.
async fn signer_as_actor(node: &EmbeddedNode, actor: &str, cid: &str) -> anyhow::Result<String> {
    let (block, signature) = node.authorized_signed_block_bytes(cid, Some(actor)).await?;
    let block = defra_core::block::Block::from_dag_cbor(&block)?;
    let signature = defra_core::block::Signature::from_dag_cbor(&signature)?;
    db::block::verify::verified_signature_signer_did(&block, &signature)
        .map_err(anyhow::Error::msg)
}

#[tokio::test]
async fn remote_client_signs_conditional_mailbox_dismissal() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let actor = crate::store::identity::KeyIdentity::load_or_create(
        directory.path().join("user.key"),
        None,
    )?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let node = Arc::new(
        EmbeddedNode::builder()
            .enable_signing()
            .with_node_acp_owner(actor.did().to_owned())
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .build()
            .await?,
    );
    let owner_context =
        defra_core::RequestContext::default().with_identity(Some(actor.did().to_owned()));
    node.add_dac_policy_with_counter_as(
        &owner_context,
        gents_protocol::schemas::MAILBOX_POLICY,
        0,
    )
    .await?;
    node.add_collection(gents_protocol::schemas::MAILBOX_ITEM)
        .await?;
    let endpoint = crate::store::config_client::GraphqlEndpoint::as_principal(
        format!("http://{address}/api/v0/graphql"),
        actor.did(),
    )
    .with_client_signing_capability(actor.client_signing_capability()?)?;
    let access = crate::ConfigAccess::Graphql(endpoint);
    let now = Utc::now().to_rfc3339();
    let input = json!({"item_key":"mailbox:one", "requester_did":actor.did(), "node_did":"did:key:node", "status":"open", "kind":"ask", "action":"ack", "title":"Review", "source_kind":"session", "source_id":"one", "target_node_did":"did:key:node", "target_agent_did":"did:key:agent", "created_at":now, "updated_at":now});
    let created = access.transact_write("test.client_mailbox_create", |txn| {
        let input = &input;
        Box::pin(async move {
            txn.execute_client_signed_create(
                "mutation($input: MailboxItemMutationInputArg!) { create_MailboxItem(input:$input) { _docID _version { cid } } }",
                &json!({"input": input}), gents_protocol::schemas::MAILBOX_ITEM,
            ).await
        })
    }).await?;
    let row = &created["data"]["add_MailboxItem"][0];
    let doc_id = row["_docID"].as_str().unwrap();
    let initial_cid = row["_version"][0]["cid"].as_str().unwrap();
    assert_eq!(signer_as_actor(&node, actor.did(), initial_cid).await?, actor.did());
    let dismissed = dismiss_mailbox_item_on(&access, doc_id).await?;
    assert_eq!(dismissed.status, "dismissed");
    let query = format!(
        r#"{{ MailboxItem(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _version {{ cid }} }} }}"#,
        escape_graphql_string(doc_id)
    );
    let updated = access.execute(&query).await?;
    let cid = updated["data"]["MailboxItem"][0]["_version"][0]["cid"]
        .as_str()
        .unwrap();
    assert_ne!(cid, initial_cid);
    assert_eq!(signer_as_actor(&node, actor.did(), cid).await?, actor.did());
    assert_eq!(
        dismiss_mailbox_item_on(&access, doc_id).await?.status,
        "dismissed"
    );
    let replay = access.execute(&query).await?;
    assert_eq!(replay["data"]["MailboxItem"][0]["_version"][0]["cid"], cid);

    let mut question = input.clone();
    question["item_key"] = json!("mailbox:question");
    question["source_id"] = json!("question");
    question["action"] = json!("start_request");
    question["payload"] = json!(json!({"version":2,"prompt":"Any constraints?","options":[],"allow_free_text":true,"optional":true}).to_string());
    let created = access.transact_write("test.client_mailbox_question", |txn| {
        let question = &question;
        Box::pin(async move {
            txn.execute_client_signed_create(
                "mutation($input: MailboxItemMutationInputArg!) { create_MailboxItem(input:$input) { _docID } }",
                &json!({"input": question}), gents_protocol::schemas::MAILBOX_ITEM,
            ).await
        })
    }).await?;
    let question_id = created["data"]["add_MailboxItem"][0]["_docID"]
        .as_str()
        .unwrap();
    let error = dismiss_mailbox_item_on(&access, question_id)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("original item"));
    node.shutdown().await?;
    Ok(())
}
