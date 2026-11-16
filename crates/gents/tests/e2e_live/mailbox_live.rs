use std::time::Duration;

use gents_documents::mailbox_write_decl::{
    canonical_mailbox_write_decl, MailboxAction, MailboxKind, MailboxNotificationPolicy,
};
use gents_documents::{DatastoreToolSurfaceDocument, DatastoreTools, SurfaceToolDecl, Tools};
use gents_protocol::mailbox_question::{
    MailboxQuestion, MailboxQuestionAnswer, MailboxQuestionOption, MAILBOX_QUESTION_VERSION,
};
use gents_protocol::operations::mailbox::{
    MailboxListInput, MailboxReply, MailboxReplyRoute, MailboxReplySource,
};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_session::operations::mailbox;
use gents_store::collection::Collection;
use gents_store::identity::NodeIdentity;
use serde_json::json;

use crate::support::agent_requests::{canonical_now, submit_user_request, RequestActors};
use crate::support::live_inference::{
    bind_target, boot_live_agent, live_target, terminal_assistant_answer, wait_for_request_terminal,
};
use crate::support::{test_db, TestDb};

const SESSION: &str = "live-mailbox-session";
const REQUEST: &str = "live-mailbox-request";

pub(crate) async fn configure(db: &TestDb, ask: bool) -> RequestActors {
    let target = live_target().await;
    let actors = bind_target(db, &target).await;
    let mut declaration = canonical_mailbox_write_decl();
    if ask {
        declaration.notification = Some(MailboxNotificationPolicy {
            kind: MailboxKind::Ask,
            action: MailboxAction::StartRequest,
            ..Default::default()
        });
    }
    crate::support::fixtures::configure_agent_tools_as(
        &actors.fixture.access,
        actors.node_did(),
        actors.agent_did(),
        Some("Follow the user's requested tool call exactly. Once it succeeds, answer briefly without calling it again. When notified of a user's answer, acknowledge it briefly.".into()),
        Tools {
            node_did: actors.node_did().into(),
            tools_id: "live-mailbox-tools".into(),
            datastore: Some(DatastoreTools {
                datastore_tool_surface_ids: Some(vec!["live-mailbox".into()]),
                ..Default::default()
            }),
            ..Default::default()
        },
        vec![(Collection::DatastoreToolSurface, serde_json::to_value(DatastoreToolSurfaceDocument {
            surface_id: "live-mailbox".into(),
            node_did: actors.node_did().into(),
            display_name: None,
            enabled: true,
            entries: Some(vec![SurfaceToolDecl::Create(declaration)]),
            created_at: None,
            tags: vec![],
        }).unwrap())],
    ).await;
    actors
}

pub(crate) async fn file(actors: &RequestActors, payload: serde_json::Value) {
    submit_user_request(actors.node_did(), &actors.fixture, SESSION, REQUEST,
        &format!("Call file_mailbox_item with exactly these arguments: {payload}. Then give a brief acknowledgement."), Default::default()).await;
    assert_eq!(
        wait_for_request_terminal(&actors.fixture.access, REQUEST, Duration::from_secs(120)).await,
        "completed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn filing_a_mailbox_item_returns_a_receipt_and_the_turn_continues() {
    let db = test_db("live-mailbox-filing").await;
    let actors = configure(&db, false).await;
    let runtime = boot_live_agent(&db, db.node_identity.clone())
        .await
        .unwrap();
    file(
        &actors,
        json!({"title":"Crew ready", "summary":"The configured agents are ready."}),
    )
    .await;
    let items = mailbox::list(
        &actors.fixture.access,
        &MailboxListInput {
            requester_did: Some(actors.fixture.user.did().into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let [item] = items.as_slice() else {
        panic!("expected the filed mailbox item: {items:?}")
    };
    assert_eq!(item.title, "Crew ready");
    assert_eq!(item.requester_did, actors.fixture.user.did());
    assert_eq!(item.target_agent_did, actors.agent_did());
    assert_eq!(item.request_id.as_deref(), Some(REQUEST));
    assert!(!terminal_assistant_answer(&actors.fixture.access, REQUEST)
        .await
        .trim()
        .is_empty());
    let outsider_path = db.data_path().join("outsider.key");
    let outsider = tokio::task::spawn_blocking(move || {
        gents_store::identity::KeyIdentity::load_or_create(outsider_path, None)
    })
    .await
    .unwrap()
    .unwrap();
    let admin = gents_store::config_client::ConfigAccess::local_as(
        db.node.clone(),
        db.node_identity.did().parse().unwrap(),
    );
    for permission in [
        gents_store::config_client::NodePermission::DocumentRead,
        gents_store::config_client::NodePermission::CollectionGet,
    ] {
        admin
            .grant_node_permission(&outsider.did().parse().unwrap(), permission)
            .await
            .unwrap();
    }
    let outside = db.client_access(&outsider);
    let hidden = outside
        .execute(&format!(
            "{{ MailboxItem(filter: {{ _docID: {{ _eq: \"{}\" }} }}) {{ _docID }} }}",
            gents_store::graphql::escape_graphql_string(&item.doc_id)
        ))
        .await
        .unwrap();
    assert!(hidden["data"]["MailboxItem"].as_array().unwrap().is_empty());
    runtime.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_optional_question_skip_reaches_the_asking_session() {
    let db = test_db("live-mailbox-question").await;
    let actors = configure(&db, true).await;
    let runtime = boot_live_agent(&db, db.node_identity.clone())
        .await
        .unwrap();
    let question = MailboxQuestion {
        version: MAILBOX_QUESTION_VERSION,
        prompt: "Ship the release?".into(),
        options: ["yes", "no"]
            .map(|id| MailboxQuestionOption {
                id: id.into(),
                label: id.into(),
                description: None,
                prompt: None,
            })
            .to_vec(),
        multi_select: false,
        allow_free_text: true,
        optional: Some(true),
        recommended_order: None,
    };
    file(
        &actors,
        json!({"title":"Release", "payload":serde_json::to_string(&question).unwrap()}),
    )
    .await;
    let items = mailbox::list(
        &actors.fixture.access,
        &MailboxListInput {
            requester_did: Some(actors.fixture.user.did().into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let [item] = items.as_slice() else {
        panic!("expected the release question: {items:?}")
    };
    assert_eq!(
        MailboxQuestion::from_payload(item.payload.as_deref().unwrap()).unwrap(),
        question
    );
    let answer = MailboxQuestionAnswer {
        skip: Some(true),
        ..Default::default()
    };
    submit_answer(&actors, item, answer, "live-mailbox-reply").await;
    assert_eq!(
        wait_for_request_terminal(
            &actors.fixture.access,
            "live-mailbox-reply",
            Duration::from_secs(120)
        )
        .await,
        "completed"
    );
    let answered = mailbox::get(&actors.fixture.access, &item.doc_id)
        .await
        .unwrap();
    assert_eq!(answered.status, "acted");
    assert!(answered.resolved_doc_id.is_some());
    assert!(
        !terminal_assistant_answer(&actors.fixture.access, "live-mailbox-reply")
            .await
            .trim()
            .is_empty()
    );
    runtime.shutdown().await;
}

async fn submit_answer(
    actors: &RequestActors,
    item: &gents_session::session::mailbox::MailboxItem,
    mut answer: MailboxQuestionAnswer,
    request_id: &str,
) {
    answer.question_hash = Some(
        gents_protocol::mailbox_question::question_source_hash(
            &item.title,
            item.summary.as_deref(),
            item.payload.as_deref().unwrap(),
            item.request_id.as_deref(),
        )
        .unwrap(),
    );
    let reply = mailbox::prepare_reply(
        MailboxReplySource {
            doc_id: &item.doc_id,
            status: &item.status,
            kind: &item.kind,
            action: &item.action,
            title: &item.title,
            summary: item.summary.as_deref(),
            payload: item.payload.as_deref(),
            request_id: item.request_id.as_deref(),
            session_id: item.session_id.as_deref(),
            target_node_did: &item.target_node_did,
            target_agent_did: &item.target_agent_did,
        },
        MailboxReply::Answer(answer),
        &MailboxReplyRoute::default(),
    )
    .unwrap();
    let mut create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        request_id,
        actors.node_did(),
        actors.fixture.user.did(),
        actors.agent_did(),
        SESSION,
        reply.content,
        "interactive",
        canonical_now(),
        AgentRequestAdmissionRecord::client(actors.fixture.user.did()),
    );
    create.caused_by_source_doc_id = Some(reply.mailbox_item_id);
    create.input.question_reply = reply.question_reply;
    create.session_doc_id = Some(
        gents_session::request::lifecycle::reserve_goal_session(&actors.fixture.access, &create)
            .await
            .unwrap(),
    );
    create.agent_doc_id = Some(
        actors
            .fixture
            .access
            .transact_readonly("test.live_mailbox.agent", |txn| {
                let actors = &actors;
                Box::pin(async move {
                    Ok::<_, anyhow::Error>(
                        gents_documents::read_desired_state_record_in_txn(
                            txn,
                            Collection::Agent,
                            actors.node_did(),
                            actors.agent_did(),
                        )
                        .await?
                        .unwrap()
                        .0,
                    )
                })
            })
            .await
            .unwrap(),
    );
    gents_session::request::admission::sign_agent_request_create(&actors.fixture.user, &mut create)
        .await
        .unwrap();
    gents_session::request::lifecycle::submit_client_request(&actors.fixture.access, &create)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_question_answered_while_its_session_is_busy_is_queued_behind_the_turn() {
    let db = test_db("live-mailbox-busy").await;
    let actors = configure(&db, true).await;
    let runtime = boot_live_agent(&db, db.node_identity.clone())
        .await
        .unwrap();
    let question = MailboxQuestion {
        version: MAILBOX_QUESTION_VERSION,
        prompt: "Continue?".into(),
        options: ["yes", "no"]
            .map(|id| MailboxQuestionOption {
                id: id.into(),
                label: id.into(),
                description: None,
                prompt: None,
            })
            .to_vec(),
        multi_select: false,
        allow_free_text: false,
        optional: Some(true),
        recommended_order: None,
    };
    file(
        &actors,
        json!({"title":"Continue", "payload":serde_json::to_string(&question).unwrap()}),
    )
    .await;
    let items = mailbox::list(
        &actors.fixture.access,
        &MailboxListInput {
            requester_did: Some(actors.fixture.user.did().into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let [item] = items.as_slice() else {
        panic!("expected the question: {items:?}")
    };
    runtime.shutdown().await;
    let target = live_target().await;
    let (runtime, held) = super::queued_user_messages::boot_held_turn(&db, &actors, &target).await;
    submit_user_request(
        actors.node_did(),
        &actors.fixture,
        SESSION,
        "live-mailbox-busy-turn",
        "Call hold_turn now. After it returns, acknowledge briefly.",
        Default::default(),
    )
    .await;
    held.wait().await;
    submit_answer(
        &actors,
        item,
        MailboxQuestionAnswer {
            option_ids: vec!["yes".into()],
            ..Default::default()
        },
        "live-mailbox-busy-reply",
    )
    .await;
    let rows = actors.fixture.access.execute(r#"{ AgentRequest(filter: {request_id: {_eq: "live-mailbox-busy-reply"}}) {_docID lifecycle_state} }"#).await.unwrap();
    let reply = &rows["data"]["AgentRequest"][0];
    assert_eq!(reply["lifecycle_state"], "pending");
    held.release();
    for id in ["live-mailbox-busy-turn", "live-mailbox-busy-reply"] {
        assert_eq!(
            wait_for_request_terminal(&actors.fixture.access, id, Duration::from_secs(120)).await,
            "completed"
        );
    }
    let answered = mailbox::get(&actors.fixture.access, &item.doc_id)
        .await
        .unwrap();
    assert_eq!(answered.status, "acted");
    assert_eq!(
        answered.resolved_doc_id.as_deref(),
        reply["_docID"].as_str()
    );
    runtime.shutdown().await;
}
