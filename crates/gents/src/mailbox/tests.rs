use std::sync::Arc;

use defra_node::EmbeddedNode;

use super::*;
use crate::identity::NodeIdentity;

const FIXTURE_CLOSE: &[MailboxCloseCollection] = &[MailboxCloseCollection {
    collection: "MailboxFixture",
    correlation_field: "mailbox_item_key",
}];

pub(super) async fn test_node() -> Arc<EmbeddedNode> {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
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
        requester_did: owner.into(),
        node_did: "did:test:agent".into(),
        agent_id: "operator".into(),
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
    dismiss_mailbox_item(&node, &first.doc_id, "did:test:owner-a")
        .await
        .unwrap();
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
            target_agent_id: "operator", expected_collection: null, parent_item_id: null,
            deadline_at: null, created_at: "{}", updated_at: "{}",
            resolved_at: null, resolved_doc_id: null
        }}) {{ _docID }} }}"#,
        Utc::now().to_rfc3339(),
        Utc::now().to_rfc3339()
    );
    let response = node.execute(&duplicate_open).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
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
    let identity = crate::identity::KeyIdentity::load_or_create(key_path, None).unwrap();
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(identity.did())
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
    let parsed = crate::lifecycle::TriggerExecutionContext::parse(Some(&trigger_context)).unwrap();
    let requester_did = parsed
        .source_fields
        .get("requester_did")
        .map(String::as_str);
    let source_item = stamp_create(
        &node,
        &context(identity.did()),
        args(MailboxAction::StartRequest, "event-observed-mailbox"),
    )
    .await
    .unwrap();
    let enqueued = crate::lifecycle::materialize::write_pending_agent_request_with_lineage_workspace_and_conversation_title(
        &node,
        ::identity::Did::new(identity.did().to_owned()).expect("fixture creator DID"),
        identity.did(),
        "operator",
        "continue the run",
        crate::lifecycle::ExecutionOrigin::Scheduled,
        crate::lifecycle::TriggerLineage {
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
            crate::request_admission::SIGNED_REQUEST_FIELDS,
        ))
        .await;
    let request = crate::watcher::AgentRequest::try_from(
        crate::graphql::rows::<gents_protocol::row::AgentRequestRow>(&response, "AgentRequest")
            .unwrap()
            .remove(0),
    )
    .unwrap();
    let request = &request;
    crate::config_client::ConfigAccess::transact_local_write(
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
        load_mailbox_item(&node, &source_item.doc_id)
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
            requester_did: "did:test:owner", agent_id: "operator",
            session_id: "session-1", content: "observe",
            lifecycle_state: "pending", execution_origin: "scheduled",
            caused_by_trigger_kind: "event", caused_by_source_doc_id: "{}",
            created_at: "{}"
        }}) {{ _docID }} }}"#,
        escape_graphql_string(&observed_item.doc_id),
        Utc::now().to_rfc3339()
    );
    assert!(!node.execute(&event_child).await.has_errors());
    let observer_report = sweep_open_mailbox_items_with_close_collections(
        &node,
        "did:test:agent",
        None,
        FIXTURE_CLOSE,
    )
    .await
    .unwrap()
    .report;
    assert_eq!(observer_report.acted, 0);
    assert_eq!(
        load_mailbox_item(&node, &observed_item.doc_id)
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
            requester_did: "did:test:owner", agent_id: "operator",
            session_id: "session-1", content: "continue",
            lifecycle_state: "pending", execution_origin: "interactive",
            caused_by_source_doc_id: "{}", created_at: "{}"
        }}) {{ _docID }} }}"#,
        escape_graphql_string(&request_item.doc_id),
        Utc::now().to_rfc3339()
    );
    assert!(!node.execute(&create_request).await.has_errors());
    let report = sweep_open_mailbox_items_with_close_collections(
        &node,
        "did:test:agent",
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
            agent_id: "operator", session_id: "session-1", content: "draft",
            caused_by_source_doc_id: "{}", created_at: "{}" }}) {{ _docID }} }}"#,
        escape_graphql_string(&domain_item.doc_id),
        Utc::now().to_rfc3339()
    );
    assert!(!node.execute(&unrelated_request).await.has_errors());
    let request_only = sweep_open_mailbox_items_with_close_collections(
        &node,
        "did:test:agent",
        None,
        FIXTURE_CLOSE,
    )
    .await
    .unwrap()
    .report;
    assert_eq!(request_only.acted, 0);
    assert_eq!(request_only.expired, 0);
    assert_eq!(
        load_mailbox_item(&node, &domain_item.doc_id)
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
        "did:test:agent",
        None,
        FIXTURE_CLOSE,
    )
    .await
    .unwrap()
    .report;
    assert_eq!(report.acted, 1);
    assert_eq!(report.expired, 0);
    let closed = load_mailbox_item(&node, &domain_item.doc_id)
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
            target_agent_id: "operator", created_at: "{now}", updated_at: "{now}"
        }}) {{ _docID }} }}"#
    );
    assert!(!node.execute(&stored).await.has_errors());

    let items = list_mailbox_items(&node, "did:test:owner", None)
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
            target_agent_id: "operator", created_at: "{now}", updated_at: "{now}"
        }}) {{ _docID }} }}"#
    );
    assert!(!node.execute(&hostile).await.has_errors());
    let undecodable = format!(
        r#"mutation {{ create_MailboxItem(input: {{
            item_key: "session:undecodable:gate:1", requester_did: "did:test:owner",
            node_did: "did:test:agent", status: "open", kind: "gate",
            action: "ack", source_kind: "session", source_id: "undecodable",
            target_node_did: "did:test:agent", target_agent_id: "operator",
            created_at: "{now}", updated_at: "{now}"
        }}) {{ _docID }} }}"#
    );
    assert!(!node.execute(&undecodable).await.has_errors());

    let mut expiring_args = args(MailboxAction::Ack, "wait-expiry");
    expiring_args.deadline_at = Some((Utc::now() - chrono::Duration::minutes(1)).to_rfc3339());
    let expiring = stamp_create(&node, &context("did:test:owner"), expiring_args)
        .await
        .unwrap();

    let report = sweep_open_mailbox_items_with_close_collections(
        &node,
        "did:test:agent",
        None,
        FIXTURE_CLOSE,
    )
    .await
    .unwrap()
    .report;
    assert_eq!(report.skipped_errors, 2);
    assert_eq!(report.expired, 1);
    assert_eq!(
        load_mailbox_item(&node, &expiring.doc_id)
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
        let mut row_context = context("did:test:owner");
        let local = row["node_did"].as_str().unwrap() == node_did;
        if !local {
            row_context.node_did = "did:test:foreign".into();
        }
        let mut row_args = args(
            MailboxAction::WriteDocument,
            &format!("model-row-{model_id:04}"),
        );
        row_args.deadline_at = Some(future.clone());
        let created =
            stamp_create_with_close_collections(&node, &row_context, row_args, FIXTURE_CLOSE)
                .await
                .unwrap();
        if local {
            modeled_doc_ids.insert(model_id, created.doc_id.clone());
            modeled_item_keys.insert(model_id, created.item_key.clone());
            modeled_ids_by_doc.insert(created.doc_id.clone(), model_id);
            modeled_ids_by_item_key.insert(created.item_key, model_id);
            created_local_ids.push(created.doc_id);
        } else {
            foreign_doc_id = Some(created.doc_id);
        }
    }

    for index in 0..64 {
        let mut closed_context = context("did:test:owner");
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
            transition_open_item(&node, &closed, MailboxStatus::Acted, None)
                .await
                .unwrap()
        );

        closed_context.node_did = "did:test:foreign".into();
        let mut foreign_args = args(
            MailboxAction::WriteDocument,
            &format!("000-foreign-{index:04}"),
        );
        foreign_args.deadline_at = Some(future.clone());
        stamp_create_with_close_collections(&node, &closed_context, foreign_args, FIXTURE_CLOSE)
            .await
            .unwrap();
    }
    let response = node
        .execute(&format!(
            r#"{{ MailboxItem(filter: {{ _and: [
                {{ node_did: {{ _eq: "{}" }} }},
                {{ status: {{ _eq: "open" }} }}
            ] }}, order: {{ item_key: ASC }}) {{ _docID item_key }} }}"#,
            escape_graphql_string("did:test:agent")
        ))
        .await;
    assert!(
        !response.has_errors(),
        "mailbox fixture order: {:?}",
        response.errors
    );
    let ordered_rows = response.data.unwrap()[MAILBOX_COLLECTION]
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
            let item = load_mailbox_item(&node, &doc_id).await.unwrap().unwrap();
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
                    load_mailbox_item(&node, &modeled_doc_ids[&model_id])
                        .await
                        .unwrap()
                        .unwrap()
                        .status,
                    "open",
                    "residual open row {model_id} should remain on either side of the cursor"
                );
            }
            let page_query = open_mailbox_page_query("did:test:agent", Some(current_cursor));
            let planned = node
                .execute(&format!("query @explain(type: simple) {page_query}"))
                .await;
            assert!(!planned.has_errors(), "mailbox simple explain: {planned:?}");
            let plan = planned.data.as_ref().unwrap().get("explain").unwrap();
            assert!(
                plan.to_string()
                    .contains("MailboxItem_node_status_item_key"),
                "mailbox page must select its registered compound index: {plan}"
            );
            let explain = node
                .execute(&format!("query @explain(type: execute) {page_query}"))
                .await;
            assert!(!explain.has_errors(), "mailbox keyset explain: {explain:?}");
            let execution = explain.data.as_ref().unwrap().get("explain").unwrap();
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
            "did:test:agent",
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
            load_mailbox_item(&node, doc_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            expected_status
        );
    }
    assert_eq!(
        load_mailbox_item(&node, foreign_doc_id.as_deref().unwrap())
            .await
            .unwrap()
            .unwrap()
            .status,
        "open"
    );
}
