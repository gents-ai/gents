use super::super::*;
use crate::client::schema::ensure_runtime_schemas;
use gents::ConfigAccess;
use gents_protocol::request_input::{QueuePolicy, QueueSource, RequestInput, RequestQueue};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use std::{collections::BTreeMap, sync::Arc};

#[tokio::test]
async fn request_refresh_recovers_modeled_turn_dependencies_without_closed_history() {
    let cases: Value = serde_json::from_str(include_str!(
        "../../../../../gents/tests/fixtures/session_request_refresh.json"
    ))
    .unwrap();
    let node = Arc::new(defra_node::NodeBuilder::default().build().await.unwrap());
    ensure_runtime_schemas(&node).await.unwrap();
    let access = ConfigAccess::local(node.clone());
    for (index, case) in cases.as_array().unwrap().iter().enumerate() {
        let session = format!("refresh-{index}");
        let request_id = |request: u64| format!("request-{index}-{request}");
        let mut docs: BTreeMap<u64, String> = BTreeMap::new();
        let rows = case["rows"].as_array().unwrap();
        for (arrival, row) in rows.iter().enumerate() {
            let doc = row["doc"].as_u64().unwrap();
            let request = row["request"].as_u64().unwrap();
            let folded = row["folded_into"].as_u64();
            let state = match row["state"].as_str().unwrap() {
                "unclaimed" => RequestLifecycleState::Pending,
                "active" => RequestLifecycleState::Processing,
                "terminal" if folded.is_some() => RequestLifecycleState::Superseded,
                "terminal" => RequestLifecycleState::Completed,
                other => panic!("unknown modeled state {other}"),
            };
            let input = row["queued_after"].as_u64().map(|after| RequestInput {
                queue: Some(RequestQueue {
                    queued_after_request_id: Some(request_id(after)),
                    source: QueueSource::User,
                    policy: QueuePolicy::Append,
                    delivery: Default::default(),
                    position: None,
                    key: None,
                    interrupted_request_id: None,
                    background_completion_wake_version: None,
                }),
                ..Default::default()
            });
            let mut create = serde_json::json!({
                "request_id": request_id(request), "purpose": "normal",
                "node_did": "did:test:node", "session_id": session,
                "agent_id": "agent", "requester_did": format!("did:test:requester:{}", row["requester"]),
                "lifecycle_state": state, "input": input,
                "created_at": format!("2026-10-15T00:00:{arrival:02}Z"),
                "retry_parent_request": row["retry_parent"].as_u64().map(|id| request_id(id)),
            });
            if let Some(owner) = folded {
                create["failure_reason"] = gents::lifecycle::FOLDED_REASON.into();
                create["superseded_by_request_doc_id"] = docs
                    .get(&owner)
                    .cloned()
                    .unwrap_or_else(|| "missing-owner".into())
                    .into();
                let owner = rows
                    .iter()
                    .find(|r| r["doc"].as_u64() == Some(owner))
                    .unwrap();
                create["superseded_by_request"] =
                    request_id(owner["request"].as_u64().unwrap()).into();
            }
            let literal = gents_protocol::graphql::graphql_input_literal(&create).unwrap();
            let response = access.write("test.request_graph.seed", &format!("mutation {{ created: create_AgentRequest(input: {literal}) {{ _docID }} }}")).await.unwrap();
            docs.insert(
                doc,
                response["data"]["created"][0]["_docID"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            );
        }
        let history = &case["history"];
        let count = history["count"].as_u64().unwrap();
        let first = history["request_start"].as_u64().unwrap();
        assert_eq!(history["state"], "terminal");
        for batch in (0..count).step_by(50) {
            let history = (batch..(batch + 50).min(count))
                .map(|offset| {
                    serde_json::json!({
                        "request_id": request_id(first + offset),
                        "purpose": "normal", "node_did": "did:test:node", "session_id": session,
                        "requester_did": format!("did:test:requester:{}", history["requester"]),
                        "lifecycle_state": "completed",
                    })
                })
                .collect::<Vec<_>>();
            let literal =
                gents_protocol::graphql::graphql_input_literal(&serde_json::json!(history))
                    .unwrap();
            access
                .write(
                    "test.request_graph.history",
                    &format!("mutation {{ create_AgentRequest(input: {literal}) {{ _docID }} }}"),
                )
                .await
                .unwrap();
        }
        let selected = request_id(rows.last().unwrap()["request"].as_u64().unwrap());
        let patch = load_chat_patch(&node, &selected, Some("did:test:node"))
            .await
            .unwrap();
        assert!(
            patch.requests.len() <= rows.len(),
            "{}: closed history was fetched",
            case["name"]
        );
        let requests = patch.requests_for_session_for_node(&session, "did:test:node");
        let newest = patch
            .request_row(&selected)
            .expect("selected request is indexed");
        assert_eq!(newest.request_id, selected);
        let turn = crate::client::session_turn_request(&requests, newest);
        assert_eq!(
            turn.doc_id.as_ref(),
            docs.get(&case["expected_turn_doc"].as_u64().unwrap()),
            "{}",
            case["name"]
        );
        let expected_queued = case["expected_queued_docs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|doc| docs[&doc.as_u64().unwrap()].clone())
            .collect::<Vec<_>>();
        assert_eq!(
            crate::client::queued_behind_turn(&requests, turn)
                .iter()
                .filter_map(|row| row.doc_id.clone())
                .collect::<Vec<_>>(),
            expected_queued,
            "{}",
            case["name"]
        );
        let mut folded = crate::client::folded_requests(&requests, newest.requester_did.as_deref())
            .iter()
            .map(|row| row.request_id.clone())
            .collect::<Vec<_>>();
        let mut expected_folded = case["expected_folded_requests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|request| request_id(request.as_u64().unwrap()))
            .collect::<Vec<_>>();
        folded.sort();
        expected_folded.sort();
        assert_eq!(folded, expected_folded, "{}", case["name"]);
        let remote = load_chat_patch_on(&access, &selected, Some("did:test:node"))
            .await
            .unwrap();
        assert_eq!(
            remote
                .requests
                .iter()
                .map(|r| &r.doc_id)
                .collect::<Vec<_>>(),
            patch.requests.iter().map(|r| &r.doc_id).collect::<Vec<_>>()
        );
    }
    node.shutdown().await;
}
