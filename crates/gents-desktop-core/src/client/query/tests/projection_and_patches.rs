use super::super::*;
use crate::client::schema::ensure_runtime_schemas;
use gents_protocol::schemas::AGENT_MESSAGE_NAME;
use std::sync::Arc;

#[tokio::test]
async fn pending_edit_request_patch_reads_only_selected_and_replacement_documents() {
    let node = Arc::new(
        crate::test_support::node_builder()
            .build()
            .await
            .expect("node"),
    );
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");
    let access = gents::ConfigAccess::local(node.clone());
    let response = access
        .write(
            "test.pending_edit_request_patch",
            r#"mutation {
                retired: create_AgentRequest(input: {
                    request_id: "edit-retired",
                    purpose: "normal",
                    node_did: "did:test:agent",
                    requester_did: "did:test:requester",
                    agent_id: "default",
                    session_id: "edit-session",
                    content: "old text",
                    lifecycle_state: "superseded",
                    execution_origin: "interactive",
                    created_at: "2026-09-01T00:00:00Z"
                }) { _docID }
                replacement: create_AgentRequest(input: {
                    request_id: "edit-replacement",
                    purpose: "normal",
                    node_did: "did:test:agent",
                    requester_did: "did:test:requester",
                    agent_id: "default",
                    session_id: "edit-session",
                    content: "new text",
                    lifecycle_state: "pending",
                    execution_origin: "interactive",
                    created_at: "2026-09-01T00:00:01Z"
                }) { _docID }
                unrelated: create_AgentRequest(input: {
                    request_id: "other-session-request",
                    purpose: "normal",
                    node_did: "did:test:agent",
                    requester_did: "did:test:requester",
                    agent_id: "default",
                    session_id: "other-session",
                    content: "unrelated",
                    lifecycle_state: "pending",
                    execution_origin: "interactive",
                    created_at: "2026-09-01T00:00:02Z"
                }) { _docID }
            }"#,
        )
        .await
        .expect("seed edit and unrelated requests");
    let doc_id = |alias: &str| {
        response["data"][alias][0]["_docID"]
            .as_str()
            .expect("physical request identity")
            .to_owned()
    };
    let selected = doc_id("retired");
    let replacement = doc_id("replacement");
    let patch = load_request_documents_on(&access, &[selected.clone(), replacement.clone()])
        .await
        .expect("exact edit request patch");

    assert_eq!(
        patch
            .iter()
            .filter_map(|row| row.doc_id.as_deref())
            .collect::<rapidhash::RapidHashSet<_>>(),
        [selected.as_str(), replacement.as_str()]
            .into_iter()
            .collect()
    );
    assert_eq!(patch.len(), 2, "unrelated session rows stay unread");
    assert!(patch.iter().any(|row| {
        row.request_id == "edit-retired"
            && row.lifecycle_state
                == Some(gents_protocol::request_lifecycle::RequestLifecycleState::Superseded)
    }));
    assert!(patch.iter().any(|row| {
        row.request_id == "edit-replacement"
            && row.lifecycle_state
                == Some(gents_protocol::request_lifecycle::RequestLifecycleState::Pending)
    }));
    let existing_unrelated = serde_json::from_value(serde_json::json!({
        "request_id": "already-local",
        "purpose": "normal",
        "node_did": "did:test:agent",
        "requester_did": "did:test:requester",
        "session_id": "another-session",
        "content": "keep this local row",
        "lifecycle_state": "pending",
        "created_at": "2026-09-01T00:00:03Z"
    }))
    .expect("unrelated local request");
    let merged = ClientStore::from_rows(ClientStoreRows {
        requests: vec![existing_unrelated],
        ..ClientStoreRows::default()
    })
    .merge_chat_patch(ClientStore::from_rows(ClientStoreRows {
        requests: patch,
        ..ClientStoreRows::default()
    }));
    assert_eq!(merged.requests.len(), 3);
    assert!(merged
        .requests
        .iter()
        .any(|row| row.request_id == "already-local"));
}

#[tokio::test]
async fn fetch_doc_patch_returns_only_matching_rows() {
    let node = Arc::new(
        crate::test_support::node_builder()
            .build()
            .await
            .expect("node"),
    );
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");

    let mutation = r#"mutation {
        create_AgentMessage(input: {
            message_key: "sess-1:1",
            session_id: "sess-1",
            node_did: "did:test:agent",
            request_doc_id: "request-sess-1",
            publication: {kind: "request_execution", execution_generation: "generation"},
            outcome: "complete",
            sequence: 1,
            role: "user",
            blocks: [],
            created_at: "2026-05-07T00:00:00Z"
        }) { _docID }
        second: create_AgentMessage(input: {
            message_key: "sess-1:2",
            session_id: "sess-1",
            node_did: "did:test:agent",
            request_doc_id: "request-sess-1",
            publication: {kind: "request_execution", execution_generation: "generation"},
            outcome: "complete",
            sequence: 2,
            role: "assistant",
            blocks: [],
            created_at: "2026-05-07T00:00:01Z"
        }) { _docID }
    }"#;
    let response = node.execute(mutation).await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let doc_ids: Vec<String> = response
        .data
        .as_ref()
        .and_then(|d| d.as_object())
        .map(|o| {
            o.values()
                .filter_map(|v| {
                    v.as_array()
                        .and_then(|a| a.first())
                        .and_then(|x| x.get("_docID"))
                        .and_then(|x| x.as_str())
                        .map(String::from)
                })
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(doc_ids.len(), 2);

    let target_id = doc_ids[0].clone();
    let patch = fetch_doc_patch(node.as_ref(), AGENT_MESSAGE_NAME, &[&target_id])
        .await
        .expect("fetch_doc_patch");
    assert_eq!(
        patch.store.transcript_messages.len(),
        1,
        "expected exactly one row"
    );
}

#[tokio::test]
async fn observer_snapshot_excludes_transcript_while_context_read_stays_authoritative() {
    let node = Arc::new(
        crate::test_support::node_builder()
            .build()
            .await
            .expect("node"),
    );
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");
    let response = node
        .execute(
            r#"mutation {
                message: create_AgentMessage(input: {
                    message_key: "resident:1",
                    session_id: "resident",
                    node_did: "did:test:selected",
                    requester_did: "did:test:local",
                    request_doc_id: "request-resident",
                    publication: {kind: "request_execution", execution_generation: "generation"},
                    outcome: "complete",
                    sequence: 1,
                    role: "user",
                    blocks: [],
                    created_at: "2026-08-26T00:00:00Z"
                }) { _docID }
                tool: create_AgentToolCall(input: {
                    tool_call_key: "resident:tool:1",
                    session_id: "resident",
                    message_sequence: 1,
                    tool_name: "read_file"
                }) { _docID }
                compaction: create_CompactionEntry(input: {
                    compaction_key: "resident:compaction:1",
                    session_id: "resident",
                    node_did: "did:test:selected",
                    requester_did: "did:test:local",
                    sequence: 1,
                    summary: "summary",
                    messages_compacted: 1,
                    compacted_through_sequence: 1,
                    original_tokens: 10,
                    compacted_tokens: 2,
                    created_at: "2026-08-26T00:00:01Z"
                }) { _docID }
            }"#,
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let observed = load_full_snapshot(node.as_ref())
        .await
        .expect("observer snapshot");
    assert!(observed.transcript_messages.is_empty());
    assert!(observed.tool_calls.is_empty());
    assert!(observed.compaction_entries.is_empty());

    let context = load_session_context_store(
        node.as_ref(),
        "resident",
        Some("did:test:selected"),
        Some("did:test:local"),
    )
    .await
    .expect("ephemeral context");
    assert_eq!(context.transcript_messages.len(), 1);
    assert_eq!(context.compaction_entries.len(), 1);
    assert_eq!(
        context.compaction_entries[0].summary.as_deref(),
        Some("summary")
    );
    let compaction_metadata = load_session_compaction_entries(
        node.as_ref(),
        "resident",
        "did:test:selected",
        Some("did:test:local"),
    )
    .await
    .expect("lightweight session compaction metadata");
    assert_eq!(compaction_metadata.len(), 1);
    assert_eq!(
        compaction_metadata[0].compaction_key,
        "resident:compaction:1"
    );
    assert_eq!(compaction_metadata[0].compacted_through_sequence, Some(1));
}

#[tokio::test]
async fn fetch_doc_patch_hydrates_mailbox_live_updates() {
    let node = Arc::new(
        crate::test_support::node_builder()
            .build()
            .await
            .expect("node"),
    );
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");

    let response = node
        .execute(
            r#"mutation {
                create_MailboxItem(input: {
                    item_key: "session:wait-live:ask:1",
                    requester_did: "did:test:owner",
                    node_did: "did:test:agent",
                    status: "open",
                    kind: "ask",
                    action: "start_request",
                    title: "Live item",
                    source_kind: "session",
                    source_id: "wait-live",
                    target_node_did: "did:test:agent",
                    target_agent_id: "operator",
                    created_at: "2026-08-25T00:00:00Z",
                    updated_at: "2026-08-25T00:00:00Z"
                }) { _docID }
            }"#,
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let lookup = node
        .execute(
            r#"query {
                MailboxItem(filter: { item_key: { _eq: "session:wait-live:ask:1" } }) {
                    _docID
                }
            }"#,
        )
        .await;
    assert!(!lookup.has_errors(), "{:?}", lookup.errors);
    let doc_id = lookup
        .data
        .as_ref()
        .and_then(|data| data.get("MailboxItem"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("_docID"))
        .and_then(Value::as_str)
        .expect("mailbox doc id");

    let patch = fetch_doc_patch(node.as_ref(), MAILBOX_ITEM_NAME, &[doc_id])
        .await
        .expect("mailbox patch");
    assert_eq!(patch.store.mailbox_items.len(), 1);
    assert_eq!(patch.store.mailbox_items[0].title, "Live item");
    assert!(supports_doc_patch_collection(MAILBOX_ITEM_NAME));
}

#[tokio::test]
async fn load_chat_patch_reads_only_the_selected_local_session() {
    let node = Arc::new(
        crate::test_support::node_builder()
            .build()
            .await
            .expect("node"),
    );
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");
    let access = gents::ConfigAccess::local(node.clone());

    let mutation = r#"mutation {
        selected_session: create_AgentSession(input: {
            session_id: "sess-selected",
            node_did: "did:test:agent",
            requester_did: "did:test:requester",
            agent_id: "default",
            created_at: "2026-07-24T00:00:00Z"
        }) { _docID }
        first_request: create_AgentRequest(input: {
            request_id: "req-selected",
            purpose: "normal",
            node_did: "did:test:agent",
            requester_did: "did:test:requester",
            agent_id: "default",
            session_id: "sess-selected",
            content: "selected",
            lifecycle_state: "processing",
            execution_generation: "generation-selected",
            created_at: "2026-07-24T00:00:00Z"
        }) { _docID }
        foreign_request_twin: create_AgentRequest(input: {
            request_id: "req-selected",
            purpose: "normal",
            node_did: "did:test:agent",
            requester_did: "did:test:foreign-requester",
            agent_id: "default",
            session_id: "sess-selected",
            content: "foreign twin",
            lifecycle_state: "processing",
            execution_generation: "generation-foreign",
            created_at: "2026-07-24T00:00:00Z"
        }) { _docID }
        second_request: create_AgentRequest(input: {
            request_id: "req-unrelated",
            purpose: "normal",
            node_did: "did:test:agent",
            requester_did: "did:test:requester",
            agent_id: "default",
            session_id: "sess-unrelated",
            content: "unrelated",
            lifecycle_state: "completed",
            created_at: "2026-07-24T00:00:00Z"
        }) { _docID }
    }"#;
    let response = node.execute(mutation).await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let selected_doc_id = response.data.as_ref().expect("mutation data")["first_request"][0]
        ["_docID"]
        .as_str()
        .expect("physical request identity");
    let observation = gents_protocol::graphql::graphql_input_literal(&serde_json::json!({
        "last_activity_at": "2026-07-24T00:00:01Z",
        "latest_request": {
            "request_doc_id": selected_doc_id,
            "request_id": "req-selected",
            "lifecycle_state": "processing"
        }
    }))
    .expect("session observation literal");
    let response = node
        .execute(&format!(
            r#"mutation {{ update_AgentSession(
                filter: {{ node_did: {{ _eq: "did:test:agent" }}, session_id: {{ _eq: "sess-selected" }} }}
                input: {{ observation: {observation} }}
            ) {{ _docID }} }}"#
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let patch = load_chat_patch(&access, "did:test:agent", "sess-selected", None, None, &[])
        .await
        .expect("selected local chat patch");
    assert_eq!(patch.requests.len(), 1);
    assert_eq!(patch.requests[0].request_id, "req-selected");
    assert_eq!(
        patch.requests[0].execution_generation.as_deref(),
        Some("generation-selected")
    );
    assert!(
        patch
            .requests
            .iter()
            .all(|row| row.session_id.as_deref() == Some("sess-selected")),
        "unrelated session leaked into selected patch"
    );

    let terminal = gents_protocol::output::TerminalOutput::Message {
        message_doc_id: "bae-terminal-message".to_string(),
    };
    let terminal_literal = gents_protocol::graphql::graphql_input_literal(
        &serde_json::to_value(&terminal).expect("serialize terminal output"),
    )
    .expect("render terminal output");
    let response = node
        .execute(&format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{request_id: {{_eq: "req-selected"}}}}
                    input: {{lifecycle_state: "completed", terminal_output: {terminal_literal}}}
                ) {{ _docID }}
            }}"#,
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let terminal_patch = load_chat_patch(
        &access,
        "did:test:agent",
        "sess-selected",
        Some("req-selected"),
        None,
        &[],
    )
    .await
    .expect("terminal selected chat patch");
    assert_eq!(terminal_patch.requests[0].terminal_output, Some(terminal));
}

#[test]
fn canonical_head_waits_for_its_exact_physical_row_without_fallback() {
    use crate::error::{BridgeError, BridgeErrorCode};

    let lookup = || super::super::SelectedRequestLookup {
        query: String::new(),
        expected_doc_id: Some("canonical-doc".into()),
        expected_request_id: Some("canonical-request".into()),
    };
    let absent = serde_json::json!({ "AgentRequest": [] });
    let error = lookup()
        .decode(&absent, "did:test:agent", "session", None)
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<BridgeError>().map(|error| error.code),
        Some(BridgeErrorCode::ProjectionPending)
    );

    let row = serde_json::json!({
        "_docID": "canonical-doc", "request_id": "canonical-request",
        "node_did": "did:test:agent", "session_id": "session",
        "requester_did": null, "purpose": "normal"
    });
    for (field, value) in [
        ("_docID", serde_json::json!("logical-twin-doc")),
        ("request_id", serde_json::json!("different-request")),
        ("node_did", serde_json::json!("did:test:other-node")),
        ("session_id", serde_json::json!("other-session")),
        ("requester_did", serde_json::json!("did:test:other-user")),
        ("purpose", Value::Null),
    ] {
        let mut changed = row.clone();
        changed[field] = value;
        let data = serde_json::json!({"AgentRequest": [changed]});
        let error = lookup()
            .decode(&data, "did:test:agent", "session", None)
            .unwrap_err();
        assert!(
            error.downcast_ref::<BridgeError>().is_none(),
            "{field} must fail rather than wait: {error}"
        );
    }
    let duplicated = serde_json::json!({"AgentRequest": [row.clone(), row.clone()]});
    assert!(lookup()
        .decode(&duplicated, "did:test:agent", "session", None)
        .unwrap_err()
        .downcast_ref::<BridgeError>()
        .is_none());
    let arrived = serde_json::json!({"AgentRequest": [row]});
    let rows = lookup()
        .decode(&arrived, "did:test:agent", "session", None)
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].doc_id.as_deref(), Some("canonical-doc"));
    assert_eq!(rows[0].request_id, "canonical-request");
}

#[test]
fn selected_request_lookup_keeps_unobserved_rows_pending() {
    use crate::error::{BridgeError, BridgeErrorCode};

    let data = serde_json::json!({ "AgentRequest": [] });
    let lookup = super::super::SelectedRequestLookup {
        query: String::new(),
        expected_doc_id: None,
        expected_request_id: None,
    };
    let error = lookup
        .decode(&data, "did:test:agent", "session", None)
        .expect_err("unobserved request is pending");
    assert_eq!(
        error.downcast_ref::<BridgeError>().map(|error| error.code),
        Some(BridgeErrorCode::ProjectionPending)
    );

    let head = super::super::SelectedRequestLookup {
        query: String::new(),
        expected_doc_id: Some("canonical-doc".to_string()),
        expected_request_id: Some("canonical-request".to_string()),
    };
    let error = head
        .decode(&data, "did:test:agent", "session", None)
        .expect_err("unobserved canonical head is pending");
    assert_eq!(
        error.downcast_ref::<BridgeError>().map(|error| error.code),
        Some(BridgeErrorCode::ProjectionPending)
    );
}

#[test]
fn selected_request_lookup_rejects_mixed_valid_and_malformed_rows() {
    let lookup = super::super::SelectedRequestLookup {
        query: String::new(),
        expected_doc_id: None,
        expected_request_id: None,
    };
    let data = serde_json::json!({
        "AgentRequest": [
            {
                "_docID": "doc-valid",
                "request_id": "valid",
                "node_did": "did:test:agent",
                "session_id": "session",
                "requester_did": null
            },
            {
                "_docID": "doc-malformed",
                "request_id": 7,
                "node_did": "did:test:agent",
                "session_id": "session",
                "requester_did": null
            }
        ]
    });

    let error = lookup
        .decode(&data, "did:test:agent", "session", None)
        .expect_err("malformed rows are invalid rather than pending");
    assert!(error.downcast_ref::<crate::error::BridgeError>().is_none());
}

#[tokio::test]
async fn load_chat_patch_uses_the_canonical_nullable_session_requester() {
    let node = Arc::new(
        crate::test_support::node_builder()
            .build()
            .await
            .expect("node"),
    );
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");
    let access = gents::ConfigAccess::local(node.clone());
    let response = node
        .execute(
            r#"mutation {
                session: create_AgentSession(input: {
                    session_id: "sess-public",
                    node_did: "did:test:agent",
                    requester_did: null,
                    agent_id: "default",
                    created_at: "2026-07-24T00:00:00Z"
                }) { _docID }
                request: create_AgentRequest(input: {
                    request_id: "req-public",
                    purpose: "normal",
                    node_did: "did:test:agent",
                    requester_did: null,
                    agent_id: "default",
                    session_id: "sess-public",
                    content: "public session",
                    lifecycle_state: "completed",
                    created_at: "2026-07-24T00:00:00Z"
                }) { _docID }
            }"#,
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let patch = load_chat_patch(
        &access,
        "did:test:agent",
        "sess-public",
        Some("req-public"),
        None,
        &[],
    )
    .await
    .expect("canonical nullable requester resolves");
    assert_eq!(patch.sessions.len(), 1);
    assert_eq!(patch.sessions[0].requester_did, None);
    assert_eq!(patch.requests.len(), 1);
    assert_eq!(patch.requests[0].requester_did, None);
}

#[tokio::test]
async fn title_request_patch_preserves_existence_without_public_projection() {
    let node = Arc::new(
        crate::test_support::node_builder()
            .build()
            .await
            .expect("node"),
    );
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");
    let access = gents::ConfigAccess::local(node.clone());
    access
        .write(
            "test.title_request_patch",
            r#"mutation {
                normal: create_AgentRequest(input: {
                    request_id: "patch-normal",
                    caused_by_parent_tool_call_doc_id: "parent-tool-physical",
                    purpose: "normal",
                    node_did: "did:test:agent",
                    agent_id: "default",
                    session_id: "patch-session",
                    content: "normal request",
                    lifecycle_state: "completed",
                    execution_origin: "interactive",
                    created_at: "2026-07-24T00:00:00Z"
                }) { _docID }
                title: create_AgentRequest(input: {
                    request_id: "patch-title",
                    purpose: "title-audit",
                    node_did: "did:test:agent",
                    agent_id: "default",
                    session_id: "patch-session",
                    content: "title audit",
                    lifecycle_state: "completed",
                    execution_origin: "interactive",
                    created_at: "2026-07-24T00:00:01Z"
                }) { _docID }
            }"#,
        )
        .await
        .expect("seed title and normal request rows");
    let rows = access
        .execute("{ AgentRequest(filter: {session_id: {_eq: \"patch-session\"}}) { _docID request_id purpose } }")
        .await
        .expect("read exact request identities");
    let rows = rows["data"]["AgentRequest"].as_array().unwrap();
    let id = |request_id: &str| {
        rows.iter()
            .find(|row| row["request_id"] == request_id)
            .and_then(|row| row["_docID"].as_str())
            .expect("created request physical ID")
    };
    let normal_id = id("patch-normal");
    let title_id = id("patch-title");
    let title_only = fetch_doc_patch(node.as_ref(), AGENT_REQUEST_NAME, &[title_id])
        .await
        .expect("title-only patch");
    assert_eq!(title_only.observed_documents, 1);
    assert!(title_only
        .missing_doc_ids
        .as_ref()
        .is_some_and(Vec::is_empty));
    assert!(title_only.store.requests.is_empty());

    let mixed = fetch_doc_patch(node.as_ref(), AGENT_REQUEST_NAME, &[normal_id, title_id])
        .await
        .expect("mixed patch");
    assert_eq!(mixed.observed_documents, 2);
    assert!(mixed.missing_doc_ids.as_ref().is_some_and(Vec::is_empty));
    assert_eq!(mixed.store.requests.len(), 1);
    assert_eq!(mixed.store.requests[0].request_id, "patch-normal");
    assert_eq!(
        mixed.store.requests[0]
            .caused_by_parent_tool_call_doc_id
            .as_deref(),
        Some("parent-tool-physical")
    );

    let missing = fetch_doc_patch(
        node.as_ref(),
        AGENT_REQUEST_NAME,
        &[title_id, "physically-absent-request"],
    )
    .await
    .expect("mixed present/missing patch");
    assert_eq!(missing.observed_documents, 1);
    assert_eq!(
        missing.missing_doc_ids.as_ref().unwrap(),
        &["physically-absent-request".to_owned()]
    );
    assert!(missing.store.requests.is_empty());
    assert!(
        missing.observed_documents < 2,
        "the observer's missing-document predicate must distinguish this batch"
    );
}

#[test]
fn request_projection_retains_every_field_needed_to_verify_signed_ancestry() {
    let projected: std::collections::BTreeSet<_> =
        AGENT_REQUEST_FIELDS.split_whitespace().collect();
    for field in gents::SIGNED_REQUEST_FIELDS.split_whitespace() {
        assert!(
            projected.contains(field),
            "request projection omits {field}"
        );
    }
}

#[tokio::test]
async fn catalog_freshness_reaches_snapshot_operator_and_document_patch_readers() -> Result<()> {
    let node = Arc::new(crate::test_support::node_builder().build().await?);
    ensure_runtime_schemas(&node).await?;
    let backend: InferenceBackend = serde_json::from_value(serde_json::json!({
        "node_did":"did:key:catalog-owner", "backend_id":"catalog-backend", "name":"Catalog",
        "provider_kind":"OpenAiCompatible", "endpoint":"http://127.0.0.1:1/v1",
        "auth":{"kind":"unauthenticated"}
    }))?;
    gents::ensure_node(&node, &backend.node_did).await?;
    let access = gents::ConfigAccess::local(node.clone());
    gents::config_client::write_inference_backend_document(&access, &backend).await?;
    for observed_at in ["2026-10-28T01:00:00Z", "2026-10-28T02:00:00Z"] {
        let catalog: gents::documents::BackendModelCatalog =
            serde_json::from_value(serde_json::json!({
                "node_did":null, "observed_at":observed_at,
                "models":[{"model_name":"retained-model"}]
            }))?;
        access
            .transact_write("test.catalog.refresh", |txn| {
                let backend = &backend;
                let catalog = catalog.clone();
                Box::pin(async move {
                    gents::inference::backends::registry::record_model_catalog_in_txn(
                        txn, backend, catalog,
                    )
                    .await
                })
            })
            .await?;
    }
    let response = access.execute("{ InferenceBackend { _docID } }").await?;
    let doc_id = response["data"]["InferenceBackend"][0]["_docID"]
        .as_str()
        .unwrap();
    let snapshot = load_full_snapshot(&node).await?;
    let operator = load_inference_configuration(&access, &backend.node_did).await?;
    let patch = fetch_doc_patch(&node, INFERENCE_BACKEND_NAME, &[doc_id])
        .await?
        .store;
    for store in [snapshot, operator, patch] {
        assert_eq!(store.backend_observations.len(), 1);
        assert_eq!(
            store.backend_observations[0].catalogs[0].observed_at,
            "2026-10-28T02:00:00Z"
        );
        assert_eq!(
            store.backend_observations[0].catalogs[0].models[0].model_name,
            "retained-model"
        );
        assert_eq!(
            store.backend_observation_source_node_dids,
            [Some(backend.node_did.clone())]
        );
        assert_eq!(store.inference_backends.len(), 1);
    }
    node.shutdown().await;
    Ok(())
}
