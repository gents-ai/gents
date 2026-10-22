use super::super::parse_query_rows;
use super::*;
use gents::config_client::ConfigAccess;
use gents_protocol::graphql::graphql_input_literal;
use gents_protocol::request_input::{QueuePolicy, QueueSource, RequestInput, RequestQueue};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::test]
async fn generated_request_closures_use_exact_database_dependencies() {
    let contract = gents_lean_contract::load_contract_snapshot::<Value>()
        .expect("executed Lean request closure contract");
    let cases = contract["client_request_closure_cases"].as_array().unwrap();
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::client::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
    let mut seeded = Vec::new();
    for case in cases {
        let session = case["name"].as_str().unwrap();
        let modeled = case["rows"].as_array().unwrap();
        let mut docs: HashMap<u64, String> = HashMap::new();
        for (index, row) in modeled.iter().enumerate() {
            let request = format!("request-{}", row["request"].as_u64().unwrap());
            let requester = row["requester"].as_u64().unwrap();
            let requester = (requester != 1).then(|| format!("requester-{requester}"));
            let state = match row["state"].as_str().unwrap() {
                "unclaimed" => "pending",
                "active" => "processing",
                "terminal" if row["folded_into"].is_number() => "superseded",
                "terminal" if row["completed"] == true => "completed",
                "terminal" => "failed",
                other => panic!("unknown modeled state {other}"),
            };
            let input = row["queued_after"].as_u64().map(|after| RequestInput {
                queue: Some(RequestQueue {
                    delivery: Default::default(),
                    position: None,
                    source: QueueSource::User,
                    policy: QueuePolicy::Append,
                    key: None,
                    queued_after_request_id: Some(format!("request-{after}")),
                    interrupted_request_id: None,
                    background_completion_wake_version: None,
                }),
                ..Default::default()
            });
            let retry_parent = row["retry_parent"]
                .as_u64()
                .map(|id| format!("request-{id}"));
            let retry_parent_doc = row["retry_parent_doc"].as_u64().map(|id| {
                docs.get(&id)
                    .expect("modeled physical parent precedes its retry")
                    .clone()
            });
            let value = json!({
                "request_id": request, "node_did":"closure-node", "session_id":session,
                "requester_did":requester, "purpose":"normal", "lifecycle_state":state,
                "created_at":format!("2026-10-15T00:00:00.{index:06}Z"), "input":input,
                "retry_parent_request":retry_parent, "retry_parent_request_doc_id":retry_parent_doc,
            });
            let input = graphql_input_literal(&value).unwrap();
            let result = ConfigAccess::write_local(
                &node,
                "test.seed_closure",
                &format!("mutation {{ create_AgentRequest(input: {input}) {{ _docID }} }}"),
            )
            .await
            .unwrap();
            docs.insert(
                row["doc"].as_u64().unwrap(),
                gents::graphql::created_doc_id(&result, "AgentRequest").unwrap(),
            );
        }
        for row in modeled {
            let Some(owner) = row["folded_into"].as_u64() else {
                continue;
            };
            let owner_request = modeled
                .iter()
                .find(|row| row["doc"].as_u64() == Some(owner))
                .unwrap()["request"]
                .as_u64()
                .unwrap();
            let doc = escape_graphql_string(&docs[&row["doc"].as_u64().unwrap()]);
            let update = graphql_input_literal(&json!({
                "superseded_by_request":format!("request-{owner_request}"),
                "superseded_by_request_doc_id":docs[&owner],
                "failure_reason":gents::lifecycle::FOLDED_REASON,
            }))
            .unwrap();
            ConfigAccess::write_local(&node, "test.seed_fold_owner", &format!(
                "mutation {{ update_AgentRequest(filter: {{_docID: {{_eq: \"{doc}\"}}}}, input: {update}) {{ _docID }} }}"
            )).await.unwrap();
        }
        seeded.push((case, docs));
    }
    for (case, docs) in seeded {
        let session = escape_graphql_string(case["name"].as_str().unwrap());
        let data = execute_local_graphql_query(&node, &format!(
            "query {{ AgentRequest(filter: {{node_did: {{_eq: \"closure-node\"}}, session_id: {{_eq: \"{session}\"}}}}) {{ {AGENT_REQUEST_FIELDS} }} }}"
        ), "test closure inputs").await.unwrap();
        let all = parse_query_rows::<AgentRequestRow>(&data, AGENT_REQUEST_NAME).unwrap();
        let seed_docs = case["seed_docs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| docs[&id.as_u64().unwrap()].as_str())
            .collect::<HashSet<_>>();
        let seeds = all
            .iter()
            .filter(|row| {
                row.doc_id
                    .as_deref()
                    .is_some_and(|doc| seed_docs.contains(doc))
            })
            .cloned()
            .collect::<Vec<_>>();
        let actual = close_request_rows(&node, seeds).await.unwrap();
        let expected = case["expected_docs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| docs[&id.as_u64().unwrap()].clone())
            .collect::<Vec<_>>();
        assert_eq!(
            actual
                .iter()
                .map(|row| row.doc_id.clone().unwrap())
                .collect::<Vec<_>>(),
            expected,
            "{}",
            case["name"]
        );
    }
}

#[tokio::test]
async fn independent_scopes_share_bounded_query_batches() {
    let seeds = (0..128)
        .map(|index| AgentRequestRow {
            doc_id: Some(format!("seed-{index}")),
            request_id: format!("request-{index}"),
            node_did: Some("node".into()),
            session_id: Some(format!("session-{index}")),
            lifecycle_state: Some(
                gents_protocol::request_lifecycle::RequestLifecycleState::Superseded,
            ),
            superseded_by_request: Some("owner".into()),
            superseded_by_request_doc_id: Some(format!("owner-{index}")),
            failure_reason: Some(gents::lifecycle::FOLDED_REASON.into()),
            ..Default::default()
        })
        .collect::<Vec<_>>();
    let seed_count = seeds.len();
    let reads = std::sync::atomic::AtomicUsize::new(0);
    let result = close_request_rows_with(seeds, |_| {
        reads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Box::pin(async {
            Ok(Value::Object(
                (0..ROOT_BATCH_SIZE)
                    .map(|index| (format!("r{index}"), json!([])))
                    .collect(),
            ))
        })
    })
    .await
    .unwrap();
    assert_eq!(result.len(), seed_count);
    assert!((1..=4).contains(&reads.load(std::sync::atomic::Ordering::Relaxed)));
}

#[tokio::test]
async fn partial_twin_seed_keeps_the_full_projection_order() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::client::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
    let queued: RequestInput = RequestInput {
        queue: Some(RequestQueue {
            delivery: Default::default(),
            position: None,
            source: QueueSource::User,
            policy: QueuePolicy::Append,
            key: None,
            queued_after_request_id: Some("twin".into()),
            interrupted_request_id: None,
            background_completion_wake_version: None,
        }),
        ..Default::default()
    };
    for (request, state, input, content) in [
        ("twin", "processing", None, "first"),
        ("twin", "processing", None, "second"),
        ("z-queued", "pending", Some(queued), "queued"),
    ] {
        let input = graphql_input_literal(&json!({
            "request_id":request, "node_did":"node", "session_id":"session",
            "created_at":"2026-10-15T00:00:00Z", "lifecycle_state":state,
            "purpose":"normal", "input":input, "content":content,
        }))
        .unwrap();
        ConfigAccess::write_local(
            &node,
            "test.seed_ordered_twins",
            &format!("mutation {{ create_AgentRequest(input: {input}) {{ _docID }} }}"),
        )
        .await
        .unwrap();
    }
    let data = execute_local_graphql_query(
        &node,
        &format!("query {{ AgentRequest {{ {AGENT_REQUEST_FIELDS} }} }}"),
        "test full twin projection",
    )
    .await
    .unwrap();
    let mut full = parse_query_rows::<AgentRequestRow>(&data, AGENT_REQUEST_NAME).unwrap();
    crate::client::store::sort_request_rows(&mut full);
    let seeds = vec![full[1].clone(), full[2].clone()];
    let closed = close_request_rows(&node, seeds).await.unwrap();
    assert_eq!(
        closed.iter().map(|row| &row.doc_id).collect::<Vec<_>>(),
        full.iter().map(|row| &row.doc_id).collect::<Vec<_>>()
    );
    let full_refs = full.iter().collect::<Vec<_>>();
    let closed_refs = closed.iter().collect::<Vec<_>>();
    assert_eq!(
        crate::client::session_turn_request(&full_refs, &full[2]).doc_id,
        crate::client::session_turn_request(&closed_refs, &closed[2]).doc_id,
    );
}

#[tokio::test]
async fn missing_scope_stays_a_leaf_without_querying() {
    let seeds = vec![AgentRequestRow {
        doc_id: Some("legacy".into()),
        ..Default::default()
    }];
    let closed = close_request_rows_with(seeds, |_| panic!("unscoped row must not query"))
        .await
        .unwrap();
    assert_eq!(closed.len(), 1);
    assert_eq!(closed[0].doc_id.as_deref(), Some("legacy"));
}

#[tokio::test]
async fn malformed_dependency_row_does_not_become_missing_owner() {
    let seeds = vec![AgentRequestRow {
        doc_id: Some("seed".into()),
        request_id: "request".into(),
        node_did: Some("node".into()),
        session_id: Some("session".into()),
        lifecycle_state: Some(gents_protocol::request_lifecycle::RequestLifecycleState::Superseded),
        superseded_by_request: Some("owner".into()),
        superseded_by_request_doc_id: Some("owner-doc".into()),
        failure_reason: Some(gents::lifecycle::FOLDED_REASON.into()),
        ..Default::default()
    }];
    let error = close_request_rows_with(seeds, |_| {
        Box::pin(async { Ok(json!({"r0":[{"request_id":[]}]})) })
    })
    .await
    .unwrap_err();
    assert!(error.to_string().contains("malformed rows"));
}
