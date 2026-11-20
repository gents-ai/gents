use std::collections::BTreeMap;

use gents_protocol::request_input::QueueDelivery;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_session::request::lifecycle::pending_user_queue;
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;

use crate::support::agent_requests::submit_user_request;
use crate::support::builder_runtime::{
    builder_actors, fixture_agent, runtime_builder, spawn_runtime,
};
use crate::support::live_inference::wait_for_request_terminal;
use crate::support::streaming_backend::{
    MockStreamingBackend, StreamPlan, StreamResponse, StreamScript,
};
use crate::support::test_db;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_queued_turns_follow_the_model_after_client_rediscovery() {
    use gents_lean_vocab::families::fold_queue_contracts_queued_turn_rediscovery as family;
    let case = family::cases::<String, String, String, String, String>()
        .into_iter()
        .find(|case| case.input.name == "ordinary_queued_inputs_arrive_during_busy_turn")
        .unwrap();
    let entries = case
        .input
        .inputs
        .iter()
        .filter_map(|event| event.entry.as_ref())
        .collect::<Vec<_>>();
    let markers = entries
        .iter()
        .map(|entry| format!("independent-queued-turn-{}", entry.request_id))
        .collect::<Vec<_>>();
    let backend = MockStreamingBackend::start_with_plans(
        "queued-turn-model",
        markers
            .iter()
            .map(|marker| {
                StreamPlan::current_authored_user(
                    marker,
                    vec![StreamResponse::Stream(StreamScript::paused(
                        marker,
                        ["independent reply"],
                    ))],
                )
            })
            .collect(),
    )
    .unwrap();
    let db = test_db("ordinary-queued-turn-rediscovery").await;
    let actors = builder_actors(
        &db,
        "queued-turn-backend",
        backend.endpoint(),
        "queued-turn-model",
        1,
    )
    .await;
    let session = case.input.session_id.to_string();
    let mut documents = BTreeMap::new();
    let first = entries[0];
    let doc = submit_user_request(
        actors.node_did(),
        &actors.fixture,
        &session,
        &first.request_id.to_string(),
        &markers[0],
        QueueDelivery::Queue,
    )
    .await;
    documents.insert(first.request_id, doc);
    let agent = fixture_agent(runtime_builder(&db, &actors), &actors, actors.agent_did())
        .backend_id("queued-turn-backend")
        .model_name("queued-turn-model")
        .stream_batch_ms(0)
        .deadline_duration_secs(60)
        .done()
        .build()
        .await
        .unwrap();
    let runtime = spawn_runtime(&db, agent).await;
    backend.wait_for_request(&markers[0]).await;
    for (entry, marker) in entries.iter().zip(&markers).skip(1) {
        assert_eq!(entry.delivery, "queue");
        let doc = submit_user_request(
            actors.node_did(),
            &actors.fixture,
            &session,
            &entry.request_id.to_string(),
            marker,
            QueueDelivery::Queue,
        )
        .await;
        documents.insert(entry.request_id, doc);
    }
    let before = pending_user_queue(
        &actors.fixture.access,
        actors.node_did(),
        &session,
        actors.fixture.user.did(),
    )
    .await
    .unwrap();
    let reconnected = db.client_access(&actors.fixture.user);
    let after = pending_user_queue(
        &reconnected,
        actors.node_did(),
        &session,
        actors.fixture.user.did(),
    )
    .await
    .unwrap();
    assert_eq!(
        serde_json::to_value(&before).unwrap(),
        serde_json::to_value(&after).unwrap()
    );
    assert_eq!(
        after
            .entries
            .iter()
            .map(|entry| &entry.request_doc_id)
            .collect::<Vec<_>>(),
        entries
            .iter()
            .skip(1)
            .map(|entry| &documents[&entry.request_id])
            .collect::<Vec<_>>()
    );
    for (index, marker) in markers.iter().enumerate().skip(1) {
        assert_eq!(backend.observed_requests(marker), 0);
        backend.release(&markers[index - 1]);
        assert_eq!(
            wait_for_request_terminal(&reconnected, &entries[index - 1].request_id.to_string())
                .await,
            "completed"
        );
        backend.wait_for_request(marker).await;
    }
    let ids = documents
        .values()
        .map(|doc| format!("\"{}\"", escape_graphql_string(doc)))
        .collect::<Vec<_>>()
        .join(",");
    let observed = reconnected.execute(&format!(r#"{{
        AgentRequest(filter: {{_docID: {{_in: [{ids}]}}}}) {{_docID request_id lifecycle_state superseded_by_request_doc_id}}
        AgentMessage(filter: {{request_doc_id: {{_in: [{ids}]}}, role: {{_eq: "user"}}}}, order: {{sequence: ASC}}) {{request_doc_id sequence}}
    }}"#)).await.unwrap();
    let rows = observed["data"]["AgentRequest"].as_array().unwrap();
    assert_eq!(rows.len(), documents.len());
    let state = |row: &serde_json::Value| {
        RequestLifecycleState::parse(row["lifecycle_state"].as_str().unwrap()).unwrap()
    };
    let mut active = rows
        .iter()
        .filter(|row| state(row).is_active_runtime())
        .map(|row| row["request_id"].as_str().unwrap().parse::<u64>().unwrap());
    let active_head = active.next();
    assert!(active.next().is_none());
    let pending = pending_user_queue(
        &reconnected,
        actors.node_did(),
        &session,
        actors.fixture.user.did(),
    )
    .await
    .unwrap();
    let mut terminal = rows
        .iter()
        .filter(|row| state(row).is_terminal())
        .map(|row| row["request_id"].as_str().unwrap().parse::<u64>().unwrap())
        .collect::<Vec<_>>();
    terminal.sort_unstable();
    let authored = observed["data"]["AgentMessage"].as_array().unwrap();
    let claims = authored
        .iter()
        .map(|message| {
            let doc = message["request_doc_id"].as_str().unwrap();
            let head = documents
                .iter()
                .find(|(_, physical)| physical.as_str() == doc)
                .unwrap()
                .0;
            let folded = rows
                .iter()
                .filter(|row| row["superseded_by_request_doc_id"].as_str() == Some(doc))
                .map(|row| row["request_id"].as_str().unwrap().parse().unwrap())
                .collect();
            family::ClaimWire {
                head: *head,
                folded,
            }
        })
        .collect();
    case.check(Some(family::FoldObservation {
        active: active_head,
        pending: pending
            .entries
            .iter()
            .map(|entry| entry.request_id.parse().unwrap())
            .collect(),
        folding: Vec::new(),
        terminal,
        claims,
    }));
    backend.release(markers.last().unwrap());
    assert_eq!(
        wait_for_request_terminal(
            &reconnected,
            &entries.last().unwrap().request_id.to_string()
        )
        .await,
        "completed"
    );
    for marker in &markers {
        assert_eq!(
            backend.observed_requests(marker),
            1,
            "one dispatch for {marker}"
        );
    }
    assert_eq!(
        backend.observed_turn_completion_bodies().len(),
        entries.len()
    );
    let replies = reconnected.execute(&format!(r#"{{ AgentMessage(filter: {{request_doc_id: {{_in: [{ids}]}}, role: {{_eq: "assistant"}}}}, order: {{sequence: ASC}}) {{request_doc_id}} }}"#)).await.unwrap();
    assert_eq!(
        replies["data"]["AgentMessage"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["request_doc_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        entries
            .iter()
            .map(|entry| documents[&entry.request_id].as_str())
            .collect::<Vec<_>>()
    );
    runtime.shutdown().await;
    db.node.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_steers_behind_a_busy_turn_are_answered_by_one_inference() {
    use crate::support::live_inference::STALL_CEILING;
    use gents_lean_vocab::families::fold_queue_contracts_steers_behind_busy_turn as family;
    let case = family::cases::<String>().into_iter().next().unwrap();
    let head = case.input.head;
    let steers = case
        .input
        .steps
        .iter()
        .filter(|step| step.kind == "enqueue_steering")
        .map(|step| step.request_id.unwrap())
        .collect::<Vec<_>>();
    assert_eq!(steers.len(), 2);
    let marker = |id: u64| format!("busy-turn-{id}");
    let head_marker = marker(head);
    let backend = MockStreamingBackend::start_with_plans(
        "busy-turn-model",
        vec![StreamPlan::new(
            &head_marker,
            vec![
                StreamResponse::Stream(StreamScript::paused(&head_marker, ["first answer"])),
                StreamResponse::completes(&head_marker, ["answer to both steers"]),
            ],
        )],
    )
    .unwrap();
    let db = test_db("busy-turn-steering").await;
    let actors = builder_actors(
        &db,
        "busy-turn-backend",
        backend.endpoint(),
        "busy-turn-model",
        1,
    )
    .await;
    let session = "busy-turn-steering";
    let head_doc = submit_user_request(
        actors.node_did(),
        &actors.fixture,
        session,
        &head.to_string(),
        &head_marker,
        QueueDelivery::Queue,
    )
    .await;
    let agent = fixture_agent(runtime_builder(&db, &actors), &actors, actors.agent_did())
        .backend_id("busy-turn-backend")
        .model_name("busy-turn-model")
        .stream_batch_ms(0)
        .done()
        .build()
        .await
        .unwrap();
    let runtime = spawn_runtime(&db, agent).await;
    backend.wait_for_request(&head_marker, STALL_CEILING).await;
    let mut steer_docs = Vec::new();
    for id in &steers {
        let doc = submit_user_request(
            actors.node_did(),
            &actors.fixture,
            session,
            &id.to_string(),
            &marker(*id),
            QueueDelivery::Steer,
        )
        .await;
        steer_docs.push((*id, doc));
    }
    backend.release(&head_marker);
    let access = &actors.fixture.access;
    assert_eq!(
        wait_for_request_terminal(access, &head.to_string(), STALL_CEILING).await,
        "completed"
    );

    let ids = std::iter::once(&head_doc)
        .chain(steer_docs.iter().map(|(_, doc)| doc))
        .map(|doc| format!("\"{}\"", escape_graphql_string(doc)))
        .collect::<Vec<_>>()
        .join(",");
    let observed = access
        .execute(&format!(
            r#"{{
        AgentRequest(filter: {{_docID: {{_in: [{ids}]}}}}) {{_docID lifecycle_state superseded_by_request_doc_id}}
        AgentMessage(filter: {{request_doc_id: {{_eq: "{head_doc}"}}, role: {{_eq: "user"}}}}, order: {{sequence: ASC}}) {{message_key}}
    }}"#,
            head_doc = escape_graphql_string(&head_doc)
        ))
        .await
        .unwrap();
    let row = |doc: &str| {
        observed["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["_docID"] == doc)
            .unwrap()
    };
    let state = |doc: &str| {
        RequestLifecycleState::parse(row(doc)["lifecycle_state"].as_str().unwrap()).unwrap()
    };
    let pending = pending_user_queue(
        access,
        actors.node_did(),
        session,
        actors.fixture.user.did(),
    )
    .await
    .unwrap()
    .entries
    .iter()
    .map(|entry| entry.request_id.parse::<u64>().unwrap())
    .collect::<Vec<_>>();
    let prefix = gents_protocol::output::rows::authored_message_key(&head_doc, "");
    let authored_keys = observed["data"]["AgentMessage"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            let key = row["message_key"]
                .as_str()
                .unwrap()
                .strip_prefix(&prefix)
                .unwrap();
            steer_docs
                .iter()
                .find(|(_, doc)| {
                    key == gents_session::request::lifecycle::queue::folded_input_key(doc)
                })
                .map_or_else(|| key.to_owned(), |(id, _)| format!("folded:{id}"))
        })
        .collect();
    case.check(Some(family::BusyTurnSteeringObservation {
        active: state(&head_doc).is_active_runtime().then_some(head),
        folding: steer_docs
            .iter()
            .filter(|(id, doc)| {
                state(doc) == RequestLifecycleState::Pending && !pending.contains(id)
            })
            .map(|(id, _)| *id)
            .collect(),
        pending,
        authored_keys,
    }));
    for (id, doc) in &steer_docs {
        assert_eq!(state(doc), RequestLifecycleState::Superseded, "steer {id}");
        assert_eq!(
            row(doc)["superseded_by_request_doc_id"],
            head_doc.as_str(),
            "steer {id} is answered by the head"
        );
    }

    let bodies = backend
        .observed_turn_completion_bodies()
        .iter()
        .map(serde_json::Value::to_string)
        .collect::<Vec<_>>();
    assert_eq!(bodies.len(), 2);
    assert!(
        steers.iter().all(|id| !bodies[0].contains(&marker(*id))),
        "the first dispatch is sent before either steer"
    );
    assert!(
        steers.iter().all(|id| bodies[1].contains(&marker(*id))),
        "the dispatch after the first carries both steers"
    );
    runtime.shutdown().await;
    db.node.shutdown().await.unwrap();
}
