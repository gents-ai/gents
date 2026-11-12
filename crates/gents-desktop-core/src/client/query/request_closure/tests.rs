use super::*;
use gents_protocol::graphql::graphql_input_literal;
use gents_protocol::request_input::{QueuePolicy, QueueSource, RequestInput, RequestQueue};
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use rapidhash::{HashMapExt, RapidHashMap};
use serde_json::json;
use serde_json::Value;
use std::sync::Arc;

#[tokio::test]
async fn generated_request_closures_use_exact_database_dependencies() {
    let contract = gents_lean_contract::load_contract_snapshot::<Value>()
        .expect("executed Lean request closure contract");
    let cases = contract["client_request_closure_cases"]
        .as_array()
        .unwrap()
        .iter()
        .chain(contract["client_session_turn_cases"].as_array().unwrap());
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::client::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
    let access = ConfigAccess::local_as(
        node.clone(),
        node.node_identity_did().unwrap().parse().unwrap(),
    );
    let outside_scope = ConfigAccess::write_local(
        &node,
        "test.seed_unavailable_successor",
        r#"mutation { create_AgentRequest(input: { purpose: "normal", request_id: "unavailable-successor", node_did: "another-node", session_id: "another-session" }) { _docID } }"#,
    )
    .await
    .unwrap();
    let missing_doc = gents_store::graphql::created_doc_id(&outside_scope, "AgentRequest").unwrap();
    let mut seeded = Vec::new();
    for (case_index, case) in cases.enumerate() {
        let session = format!("{case_index}-{}", case["name"].as_str().unwrap());
        let modeled = case["rows"].as_array().unwrap();
        let mut docs: RapidHashMap<u64, String> = RapidHashMap::new();
        for (index, row) in modeled.iter().enumerate() {
            let request = format!("request-{}", row["request"].as_u64().unwrap());
            let requester = row["requester"].as_u64().unwrap();
            let requester = (requester != 1).then(|| format!("requester-{requester}"));
            let state = match row["state"].as_str().unwrap() {
                "unclaimed" => "pending",
                "active" => "processing",
                "terminal" if row["folded_into"].is_number() || row["replaced_by"].is_number() => {
                    "superseded"
                }
                "terminal" if row["completed"] == true => "completed",
                "terminal" => "failed",
                other => panic!("unknown modeled state {other}"),
            };
            let background_wake = row["background_wake"] == true;
            let input =
                (row["queued_after"].is_number() || background_wake).then(|| RequestInput {
                    queue: Some(RequestQueue {
                        delivery: Default::default(),
                        position: None,
                        source: if background_wake {
                            QueueSource::BackgroundCompletion
                        } else {
                            QueueSource::User
                        },
                        policy: QueuePolicy::Append,
                        key: None,
                        queued_after_request_id: row["queued_after"]
                            .as_u64()
                            .map(|after| format!("request-{after}")),
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
                "requester_did":requester,
                "purpose": if row["public_input"] == false {
                    gents_protocol::request_admission::RequestPurpose::TitleAudit.as_str()
                } else { gents_protocol::request_admission::RequestPurpose::Normal.as_str() },
                "lifecycle_state":state,
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
                gents_store::graphql::created_doc_id(&result, "AgentRequest").unwrap(),
            );
        }
        for row in modeled {
            let Some(owner) = row["folded_into"].as_u64().or(row["replaced_by"].as_u64()) else {
                continue;
            };
            let owner_request = modeled
                .iter()
                .find(|row| row["doc"].as_u64() == Some(owner))
                .and_then(|row| row["request"].as_u64())
                .unwrap_or(owner);
            let doc = escape_graphql_string(&docs[&row["doc"].as_u64().unwrap()]);
            let update = graphql_input_literal(&json!({
                "superseded_by_request":format!("request-{owner_request}"),
                "superseded_by_request_doc_id":docs.get(&owner).cloned().unwrap_or_else(|| missing_doc.clone()),
                "failure_reason":if row["folded_into"].is_number() {
                    gents_session::request::lifecycle::FOLDED_REASON
                } else {
                    gents_session::request::lifecycle::REPLACED_REASON
                },
            }))
            .unwrap();
            ConfigAccess::write_local(&node, "test.seed_fold_owner", &format!(
                "mutation {{ update_AgentRequest(filter: {{_docID: {{_eq: \"{doc}\"}}}}, input: {update}) {{ _docID }} }}"
            )).await.unwrap();
        }
        seeded.push((case, docs, session));
    }
    let mut excluded_sql_cases = Vec::new();
    let mut sql_case_count = 0;
    for (case, docs, session) in seeded {
        let all = access
            .read_view(
                &RequestRows::new(gents_session::views::transcript::Scope::Node(
                    "closure-node".into(),
                ))
                .session_id(Some(session.clone())),
            )
            .await
            .unwrap();
        let seed_docs = case["seed_docs"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|id| docs[&id.as_u64().unwrap()].as_str())
            .collect::<RapidHashSet<_>>();
        let mut seeds = all
            .iter()
            .filter(|row| {
                row.doc_id
                    .as_deref()
                    .is_some_and(|doc| seed_docs.contains(doc))
            })
            .cloned()
            .collect::<Vec<_>>();
        if let Some(selected_doc) = case["selected_doc"]
            .as_u64()
            .filter(|_| case["native_completion_premise"] == true)
        {
            sql_case_count += 1;
            let selected = all
                .iter()
                .find(|row| row.doc_id.as_ref() == Some(&docs[&selected_doc]))
                .unwrap()
                .clone();
            let page_docs = case["page_request_docs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|doc| docs[&doc.as_u64().unwrap()].clone())
                .collect::<Vec<_>>();
            let scopes = super::super::request_seed_scopes(std::slice::from_ref(&selected));
            seeds = access
                .transact_readonly("test.request_seed_views", |txn| {
                    let selected = selected.clone();
                    let scopes = &scopes;
                    let page_docs = &page_docs;
                    Box::pin(async move {
                        super::super::read_request_seeds_in_txn(
                            txn,
                            vec![selected],
                            scopes,
                            page_docs,
                        )
                        .await
                    })
                })
                .await
                .unwrap();
        } else if case["selected_doc"].is_number() {
            excluded_sql_cases.push(case["name"].as_str().unwrap());
        }
        let actual = close_request_rows_on(&access, seeds).await.unwrap();
        let expected = case
            .get("expected_read_docs")
            .unwrap_or(&case["expected_docs"])
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
    assert_eq!(
        excluded_sql_cases,
        ["completed_without_authored_input_is_retained"]
    );
    assert_eq!(
        sql_case_count + excluded_sql_cases.len(),
        contract["client_session_turn_cases"]
            .as_array()
            .unwrap()
            .len(),
        "every modeled session query case binds SQL or its explicit publication-premise refusal"
    );
}

#[tokio::test]
async fn partial_twin_seed_keeps_the_full_projection_order() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::client::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
    let access = ConfigAccess::local_as(
        node.clone(),
        node.node_identity_did().unwrap().parse().unwrap(),
    );
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
    let mut full = access
        .read_view(&RequestRows::new(
            gents_session::views::transcript::Scope::Node("node".into()),
        ))
        .await
        .unwrap();
    crate::client::store::sort_request_rows(&mut full);
    let seeds = vec![full[1].clone(), full[2].clone()];
    let closed = close_request_rows_on(&access, seeds).await.unwrap();
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
