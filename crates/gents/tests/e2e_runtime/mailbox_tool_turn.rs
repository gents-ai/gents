use std::sync::Arc;
use std::time::Duration;

use gents::document_config::{DatastoreTools, SurfaceToolDecl, Tools};
use gents::mailbox::{canonical_mailbox_write_decl, list_mailbox_items, MailboxStatus};
use gents::{AgentIdentity, Collection, DatastoreToolSurfaceDocument};

use crate::support::accepted_turn::{
    boot_prepared_accepted_turn, prepare_accepted_turn, AcceptedTurnSpec,
};
use crate::support::fixtures::configure_behavior_tools;
use crate::support::interrupt::create_runtime_request_caused_by_source;
use crate::support::live_inference::wait_for_request_terminal;
use crate::support::streaming_backend::{StreamChunk, StreamPlan, StreamResponse};
use crate::support::test_db;

const BEHAVIOR: &str = "mailbox-turn-engineer";
const SURFACE: &str = "engineer-mailbox";

/// Filing an informational mailbox item through a surface shaped like the
/// Engineer's (`gents init --setup-steward`) is an ordinary tool call: the
/// receipt reaches the model and the request completes on the next turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn filing_a_mailbox_item_returns_a_receipt_and_the_turn_continues() {
    let db = test_db("mailbox-tool-turn").await;
    let did = db.node_identity.did().to_string();
    let request_id = "mailbox-tool-turn-request";
    let prompt = "mailbox-tool-turn-prompt";
    let prepared = prepare_accepted_turn(
        &db,
        AcceptedTurnSpec {
            backend_id: "mailbox-tool-turn-backend",
            model: "mailbox-tool-turn-model",
            parent_behavior_id: BEHAVIOR,
            configured_behavior_ids: &[BEHAVIOR],
            request_id,
            session_id: "mailbox-tool-turn-session",
            prompt,
            accepted_chunks: vec![StreamChunk::tool_call(
                "mailbox-tool-turn-call",
                gents::mailbox::FILE_MAILBOX_ITEM_TOOL_NAME,
                serde_json::json!({
                    "title": "Crew ready",
                    "summary": "Two agents configured.\n\n- builder\n- reviewer",
                })
                .to_string(),
            )],
            child_plans: Vec::new(),
            valid_until: None,
            subagent_depth: None,
            request_setup: None,
        },
    )
    .await;
    configure_behavior_tools(
        db.node.as_ref(),
        &did,
        BEHAVIOR,
        None,
        Tools {
            tools_id: format!("{BEHAVIOR}:tools"),
            agent_did: did.clone(),
            datastore: Some(DatastoreTools {
                enable_defra_query: Some(true),
                datastore_tool_surface_ids: Some(vec![SURFACE.into()]),
                ..Default::default()
            }),
            subagents: Some(gents::document_config::SubagentTools {
                target_ids: Vec::new(),
                enabled: Some(true),
            }),
            built_ins: Some(gents::document_config::BuiltInTools {
                enable_session_history_tool: Some(true),
                enable_graph_tools: Some(true),
                ..Default::default()
            }),
            self_config: Some(gents::agent::persona_ops::setup_steward_self_config()),
            ..Default::default()
        },
        vec![(
            Collection::DatastoreToolSurface,
            serde_json::to_value(DatastoreToolSurfaceDocument {
                surface_id: SURFACE.into(),
                agent_did: did.clone(),
                display_name: Some("Engineer escalations".into()),
                enabled: true,
                entries: Some(vec![
                    SurfaceToolDecl::Create(canonical_mailbox_write_decl()),
                ]),
                created_at: None,
                tags: Vec::new(),
            })
            .unwrap(),
        )],
    )
    .await;
    let identity: Arc<dyn AgentIdentity> = db.node_identity.clone();
    let agent = gents::Gents::from_default_behavior_documents(
        db.node.clone(),
        identity,
        gents::DocumentRuntimeOptions::default(),
    )
    .await
    .unwrap();
    let runtime = boot_prepared_accepted_turn(&db, prepared, agent).await;

    let state =
        wait_for_request_terminal(db.node.as_ref(), request_id, Duration::from_secs(60)).await;
    let failure = db
        .node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{ lifecycle_state failure_reason }} }}"#
        ))
        .await;
    let tool_calls = db
        .node
        .execute(r#"{ AgentToolCall { tool_name lifecycle_state status tool_failure_class } }"#)
        .await;
    assert_eq!(
        state, "completed",
        "request: {:?}; tool calls: {:?} {:?}",
        failure.data, tool_calls.data, tool_calls.errors
    );
    let items = list_mailbox_items(db.node.as_ref(), &did, Some(MailboxStatus::Open))
        .await
        .unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].title, "Crew ready");
    // The first filing's outcome comes from the Lean-bound notification owner.
    let outcome = serde_json::to_value(
        gents::mailbox::NotificationIdentity::Event.write_outcome(false, false),
    )
    .unwrap();
    let bodies = runtime.backend.observed_completion_bodies();
    let followup = bodies.last().expect("follow-up provider turn");
    let receipt_text = followup["messages"]
        .as_array()
        .expect("follow-up messages")
        .iter()
        .find(|message| {
            message["role"] == "tool" && message["tool_call_id"] == "mailbox-tool-turn-call"
        })
        .and_then(|message| message["content"].as_str())
        .expect("the matching mailbox receipt must reach the model");
    let receipt: serde_json::Value =
        serde_json::from_str(receipt_text).expect("mailbox receipt must be valid JSON");
    assert_eq!(receipt["outcome"], outcome);
    assert_eq!(receipt["item"]["_docID"], items[0].doc_id);
    assert_eq!(receipt["item"]["title"], items[0].title);
    assert_eq!(receipt["item"]["request_id"], request_id);
    runtime.shutdown().await;
}

pub(super) async fn configure_engineer_mailbox(db: &crate::support::TestDb, did: &str) {
    configure_behavior_tools(
        db.node.as_ref(),
        did,
        BEHAVIOR,
        None,
        Tools {
            tools_id: format!("{BEHAVIOR}:tools"),
            agent_did: did.to_string(),
            datastore: Some(DatastoreTools {
                datastore_tool_surface_ids: Some(vec![SURFACE.into()]),
                ..Default::default()
            }),
            ..Default::default()
        },
        vec![(
            Collection::DatastoreToolSurface,
            serde_json::to_value(DatastoreToolSurfaceDocument {
                surface_id: SURFACE.into(),
                agent_did: did.to_string(),
                display_name: Some("Engineer escalations".into()),
                enabled: true,
                entries: Some(vec![
                    SurfaceToolDecl::Create(canonical_mailbox_write_decl()),
                ]),
                created_at: None,
                tags: Vec::new(),
            })
            .unwrap(),
        )],
    )
    .await;
}

/// A question filed without waiting is answered by the item's ordinary reply
/// request: the claim consumes the item and the answer reaches the model as
/// the session's next user message.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_filed_question_is_answered_through_the_reply_request() {
    use gents_protocol::mailbox_question::{
        MailboxQuestion, MailboxQuestionAnswer, MailboxQuestionOption, MAILBOX_QUESTION_VERSION,
    };
    let db = test_db("mailbox-question-turn").await;
    let did = db.node_identity.did().to_string();
    let session_id = "mailbox-question-session";
    let question = MailboxQuestion {
        version: MAILBOX_QUESTION_VERSION,
        prompt: "Ship the release?".into(),
        options: ["yes", "no"]
            .map(|id| MailboxQuestionOption {
                id: id.into(),
                label: id.to_uppercase(),
                description: None,
            })
            .to_vec(),
        multi_select: false,
        allow_free_text: true,
    };
    let reply = question
        .reply_content(
            "Release",
            &MailboxQuestionAnswer {
                option_ids: vec!["yes".into()],
                free_text: Some("after the tag".into()),
            },
        )
        .unwrap();
    let prepared = prepare_accepted_turn(
        &db,
        AcceptedTurnSpec {
            backend_id: "mailbox-question-backend",
            model: "mailbox-question-model",
            parent_behavior_id: BEHAVIOR,
            configured_behavior_ids: &[BEHAVIOR],
            request_id: "mailbox-question-request",
            session_id,
            prompt: "mailbox-question-prompt",
            accepted_chunks: vec![StreamChunk::tool_call(
                "mailbox-question-call",
                gents::mailbox::FILE_MAILBOX_ITEM_TOOL_NAME,
                serde_json::json!({"title": "Release", "question": question}).to_string(),
            )],
            child_plans: vec![StreamPlan::new(
                reply.clone(),
                vec![StreamResponse::completes(reply.clone(), ["noted"])],
            )],
            valid_until: None,
            subagent_depth: None,
            request_setup: None,
        },
    )
    .await;
    configure_engineer_mailbox(&db, &did).await;
    let identity: Arc<dyn AgentIdentity> = db.node_identity.clone();
    let agent = gents::Gents::from_default_behavior_documents(
        db.node.clone(),
        identity,
        gents::DocumentRuntimeOptions::default(),
    )
    .await
    .unwrap();
    let runtime = boot_prepared_accepted_turn(&db, prepared, agent).await;
    assert_eq!(
        wait_for_request_terminal(
            db.node.as_ref(),
            "mailbox-question-request",
            Duration::from_secs(60)
        )
        .await,
        "completed",
        "filing a question must not suspend or fail the asking turn"
    );
    let items = list_mailbox_items(db.node.as_ref(), &did, Some(MailboxStatus::Open))
        .await
        .unwrap();
    assert_eq!(items.len(), 1);
    let item = &items[0];
    assert_eq!(
        (item.kind.as_str(), item.action.as_str()),
        ("ask", "start_request")
    );
    assert_eq!(item.session_id.as_deref(), Some(session_id));
    assert_eq!(
        MailboxQuestion::from_payload(item.payload.as_deref().unwrap()).unwrap(),
        question
    );

    let reply_doc = create_runtime_request_caused_by_source(
        db.node.as_ref(),
        &did,
        BEHAVIOR,
        "mailbox-question-reply",
        session_id,
        &item.doc_id,
        &reply,
    )
    .await;
    assert_eq!(
        wait_for_request_terminal(
            db.node.as_ref(),
            "mailbox-question-reply",
            Duration::from_secs(60)
        )
        .await,
        "completed"
    );
    let answered = gents::mailbox::load_mailbox_item(db.node.as_ref(), &item.doc_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(answered.status, "acted");
    assert_eq!(
        answered.resolved_doc_id.as_deref(),
        Some(reply_doc.as_str())
    );
    let bodies = runtime.backend.observed_completion_bodies();
    let delivered = bodies.last().expect("reply provider turn").to_string();
    assert!(
        delivered.contains("Decision on Release: YES (yes)") && delivered.contains("after the tag"),
        "the answer must reach the model in the asking session: {delivered}"
    );
    runtime.shutdown().await;
}

pub(super) async fn request_state(
    node: &gents::defra_node::EmbeddedNode,
    request_id: &str,
) -> Option<String> {
    let response = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{ lifecycle_state }} }}"#
        ))
        .await;
    response.data.as_ref()?["AgentRequest"]
        .as_array()?
        .first()?
        .get("lifecycle_state")?
        .as_str()
        .map(str::to_owned)
}

/// The person answers while the asking turn is still running: the reply is an
/// ordinary interactive request into the busy session, queued behind the
/// active turn, and still resolves the item through the reply claim.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_question_answered_while_its_session_is_busy_is_queued_behind_the_turn() {
    use gents_protocol::mailbox_question::{
        MailboxQuestion, MailboxQuestionAnswer, MailboxQuestionOption, MAILBOX_QUESTION_VERSION,
    };
    let db = test_db("mailbox-question-busy").await;
    let did = db.node_identity.did().to_string();
    let session_id = "mailbox-question-busy-session";
    let asking = "mailbox-question-busy-request";
    let prompt = "mailbox-question-busy-prompt";
    let question = MailboxQuestion {
        version: MAILBOX_QUESTION_VERSION,
        prompt: "Keep going?".into(),
        options: ["yes", "no"]
            .map(|id| MailboxQuestionOption {
                id: id.into(),
                label: id.to_uppercase(),
                description: None,
            })
            .to_vec(),
        multi_select: false,
        allow_free_text: false,
    };
    let reply = question
        .reply_content(
            "Continue",
            &MailboxQuestionAnswer {
                option_ids: vec!["yes".into()],
                free_text: None,
            },
        )
        .unwrap();
    let prepared = prepare_accepted_turn(
        &db,
        AcceptedTurnSpec {
            backend_id: "mailbox-question-busy-backend",
            model: "mailbox-question-busy-model",
            parent_behavior_id: BEHAVIOR,
            configured_behavior_ids: &[BEHAVIOR],
            request_id: asking,
            session_id,
            prompt,
            accepted_chunks: vec![StreamChunk::tool_call(
                "mailbox-question-busy-call",
                gents::mailbox::FILE_MAILBOX_ITEM_TOOL_NAME,
                serde_json::json!({"title": "Continue", "question": question}).to_string(),
            )],
            child_plans: vec![StreamPlan::new(
                reply.clone(),
                vec![StreamResponse::completes(reply.clone(), ["noted"])],
            )],
            valid_until: None,
            subagent_depth: None,
            request_setup: None,
        },
    )
    .await;
    // The asking turn keeps working: its next provider turn waits on the test.
    prepared.backend.enable_dynamic_followups(prompt);
    configure_engineer_mailbox(&db, &did).await;
    let identity: Arc<dyn AgentIdentity> = db.node_identity.clone();
    let agent = gents::Gents::from_default_behavior_documents(
        db.node.clone(),
        identity,
        gents::DocumentRuntimeOptions::default(),
    )
    .await
    .unwrap();
    let runtime = boot_prepared_accepted_turn(&db, prepared, agent).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let item = loop {
        let open = list_mailbox_items(db.node.as_ref(), &did, Some(MailboxStatus::Open))
            .await
            .unwrap();
        if let Some(item) = open.into_iter().next() {
            break item;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "question never filed"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    assert!(
        !gents_protocol::request_lifecycle::RequestLifecycleState::is_terminal_str(
            request_state(db.node.as_ref(), asking).await.as_deref()
        )
    );

    // The desktop bridge's shape for an answer into a busy session: a queued
    // user turn behind the active request.
    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        "mailbox-question-busy-reply",
        &did,
        &did,
        BEHAVIOR,
        session_id,
        &reply,
        "interactive",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(&did),
    );
    create.caused_by_source_doc_id = Some(item.doc_id.clone());
    create.input.queue = Some(gents_protocol::request_input::RequestQueue {
        source: gents_protocol::request_input::QueueSource::User,
        policy: gents_protocol::request_input::QueuePolicy::Append,
        key: None,
        queued_after_request_id: Some(asking.into()),
        interrupted_request_id: None,
        background_completion_wake_version: None,
    });
    gents::sign_agent_request_create_as_registered_target(&mut create)
        .await
        .unwrap();
    let created = db.node.execute(&create.graphql_mutation().unwrap()).await;
    assert!(!created.has_errors(), "{:?}", created.errors);
    let reply_doc =
        crate::support::exact_request_doc_id(db.node.as_ref(), "mailbox-question-busy-reply").await;
    // Admitted and waiting behind the running turn, not refused or failed.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        request_state(db.node.as_ref(), "mailbox-question-busy-reply")
            .await
            .as_deref(),
        Some("pending")
    );
    assert!(
        !gents_protocol::request_lifecycle::RequestLifecycleState::is_terminal_str(
            request_state(db.node.as_ref(), asking).await.as_deref()
        )
    );
    runtime
        .backend
        .enqueue_response(prompt, StreamResponse::completes(prompt, ["asking done"]));
    let asking_final =
        wait_for_request_terminal(db.node.as_ref(), asking, Duration::from_secs(60)).await;
    let reply_final = wait_for_request_terminal(
        db.node.as_ref(),
        "mailbox-question-busy-reply",
        Duration::from_secs(60),
    )
    .await;
    assert_eq!(asking_final, "completed");
    assert_eq!(reply_final, "completed");
    let answered = gents::mailbox::load_mailbox_item(db.node.as_ref(), &item.doc_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(answered.status, "acted");
    assert_eq!(
        answered.resolved_doc_id.as_deref(),
        Some(reply_doc.as_str())
    );
    let bodies = runtime.backend.observed_completion_bodies();
    assert!(
        bodies
            .last()
            .unwrap()
            .to_string()
            .contains("Decision on Continue: YES (yes)"),
        "the answer must reach the model as the session's next turn"
    );
    runtime.shutdown().await;
}
