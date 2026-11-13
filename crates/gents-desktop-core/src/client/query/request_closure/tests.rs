use super::*;
use gents_protocol::graphql::graphql_input_literal;
use gents_protocol::request_input::{QueuePolicy, QueueSource, RequestInput, RequestQueue};
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use rapidhash::{HashMapExt, RapidHashMap};
use serde_json::json;
use std::sync::Arc;

use gents_lean_vocab::families::client_request_closure_closed as closed_family;
use gents_lean_vocab::families::client_shell_contracts_session_turn_cases_read as read_family;

async fn seed_modeled_rows(
    node: &Arc<defra_node::Node>,
    session: &str,
    modeled: &[closed_family::Row<String>],
    missing_doc: &str,
) -> RapidHashMap<u64, String> {
    let mut docs: RapidHashMap<u64, String> = RapidHashMap::new();
    for (index, row) in modeled.iter().enumerate() {
        let requester = (row.requester != 1).then(|| format!("requester-{}", row.requester));
        let state = match row.state.as_str() {
            "unclaimed" => "pending",
            "active" => "processing",
            "terminal" if row.folded_into.is_some() || row.replaced_by.is_some() => "superseded",
            "terminal" if row.completed => "completed",
            "terminal" => "failed",
            other => panic!("unknown modeled state {other}"),
        };
        let input = (row.queued_after.is_some() || row.background_wake).then(|| RequestInput {
            queue: Some(RequestQueue {
                delivery: Default::default(),
                position: None,
                source: if row.background_wake {
                    QueueSource::BackgroundCompletion
                } else {
                    QueueSource::User
                },
                policy: QueuePolicy::Append,
                key: None,
                queued_after_request_id: row.queued_after.map(|after| format!("request-{after}")),
                interrupted_request_id: None,
                background_completion_wake_version: None,
            }),
            ..Default::default()
        });
        let retry_parent_doc = row.retry_parent_doc.map(|id| {
            docs.get(&id)
                .expect("modeled physical parent precedes its retry")
                .clone()
        });
        let value = json!({
            "request_id": format!("request-{}", row.request), "node_did":"closure-node",
            "session_id":session, "requester_did":requester,
            "purpose": if row.public_input {
                gents_protocol::request_admission::RequestPurpose::Normal.as_str()
            } else {
                gents_protocol::request_admission::RequestPurpose::TitleAudit.as_str()
            },
            "lifecycle_state":state,
            "created_at":format!("2026-10-15T00:00:00.{index:06}Z"), "input":input,
            "retry_parent_request":row.retry_parent.map(|id| format!("request-{id}")),
            "retry_parent_request_doc_id":retry_parent_doc,
        });
        let input = graphql_input_literal(&value).unwrap();
        let result = ConfigAccess::write_local(
            node,
            "test.seed_closure",
            &format!("mutation {{ create_AgentRequest(input: {input}) {{ _docID }} }}"),
        )
        .await
        .unwrap();
        docs.insert(
            row.doc,
            gents_store::graphql::created_doc_id(&result, "AgentRequest").unwrap(),
        );
    }
    for row in modeled {
        let Some(owner) = row.folded_into.or(row.replaced_by) else {
            continue;
        };
        let owner_request = modeled
            .iter()
            .find(|candidate| candidate.doc == owner)
            .map_or(owner, |candidate| candidate.request);
        let doc = escape_graphql_string(&docs[&row.doc]);
        let update = graphql_input_literal(&json!({
            "superseded_by_request":format!("request-{owner_request}"),
            "superseded_by_request_doc_id":docs.get(&owner).cloned().unwrap_or_else(|| missing_doc.to_owned()),
            "failure_reason":if row.folded_into.is_some() {
                gents_session::request::lifecycle::FOLDED_REASON
            } else {
                gents_session::request::lifecycle::REPLACED_REASON
            },
        }))
        .unwrap();
        ConfigAccess::write_local(node, "test.seed_fold_owner", &format!(
            "mutation {{ update_AgentRequest(filter: {{_docID: {{_eq: \"{doc}\"}}}}, input: {update}) {{ _docID }} }}"
        )).await.unwrap();
    }
    docs
}

async fn session_rows(access: &ConfigAccess, session: &str) -> Vec<AgentRequestRow> {
    access
        .read_view(
            &RequestRows::new(gents_session::views::transcript::Scope::Node(
                "closure-node".into(),
            ))
            .session_id(Some(session.to_owned())),
        )
        .await
        .unwrap()
}

fn modeled_docs(docs: &RapidHashMap<u64, String>, rows: &[AgentRequestRow]) -> Vec<u64> {
    rows.iter()
        .map(|row| {
            let doc = row.doc_id.as_deref().unwrap();
            docs.iter()
                .find(|(_, seeded)| seeded.as_str() == doc)
                .map(|(modeled, _)| *modeled)
                .unwrap_or_else(|| panic!("closure returned unseeded row {doc}"))
        })
        .collect()
}

#[tokio::test]
async fn generated_request_closures_use_exact_database_dependencies() {
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
    for (case_index, case) in
        gents_lean_vocab::families::client_request_closure_closed::cases::<String>()
            .iter()
            .enumerate()
    {
        let session = format!("closed-{case_index}-{}", case.input.name);
        let docs = seed_modeled_rows(&node, &session, &case.input.rows, &missing_doc).await;
        let seed_docs = case
            .input
            .seed_docs
            .iter()
            .map(|doc| docs[doc].as_str())
            .collect::<RapidHashSet<_>>();
        let seeds = session_rows(&access, &session)
            .await
            .into_iter()
            .filter(|row| {
                row.doc_id
                    .as_deref()
                    .is_some_and(|doc| seed_docs.contains(doc))
            })
            .collect::<Vec<_>>();
        let closed_rows = close_request_rows_on(&access, seeds).await.unwrap();
        case.check(closed_family::ClosedOutput {
            docs: modeled_docs(&docs, &closed_rows),
        });
    }
    for (case_index, case) in
        gents_lean_vocab::families::client_shell_contracts_session_turn_cases_read::cases::<String>(
        )
        .iter()
        .enumerate()
    {
        let session = format!("read-{case_index}-{}", case.input.name);
        let modeled: Vec<closed_family::Row<String>> =
            gents_lean_vocab::family::recode(&case.input.rows);
        let docs = seed_modeled_rows(&node, &session, &modeled, &missing_doc).await;
        let all = session_rows(&access, &session).await;
        let selected = all
            .iter()
            .find(|row| row.doc_id.as_ref() == Some(&docs[&case.input.selected_doc]))
            .unwrap()
            .clone();
        let page_docs = case
            .input
            .page_request_docs
            .iter()
            .map(|doc| docs[doc].clone())
            .collect::<Vec<_>>();
        let scopes = super::super::request_seed_scopes(std::slice::from_ref(&selected));
        let seeds = access
            .transact_readonly("test.request_seed_views", |txn| {
                let selected = selected.clone();
                let scopes = &scopes;
                let page_docs = &page_docs;
                Box::pin(async move {
                    super::super::read_request_seeds_in_txn(txn, vec![selected], scopes, page_docs)
                        .await
                })
            })
            .await
            .unwrap();
        let read_rows = close_request_rows_on(&access, seeds).await.unwrap();
        case.check(read_family::ReadOutput {
            read_docs: modeled_docs(&docs, &read_rows),
        });
    }
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
