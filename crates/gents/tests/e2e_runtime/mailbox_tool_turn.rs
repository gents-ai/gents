use std::sync::Arc;
use std::time::Duration;

use gents_store::identity::NodeIdentity;

use crate::support::accepted_turn::boot_prepared_accepted_turn;
use crate::support::streaming_backend::{StreamChunk, StreamPlan, StreamResponse};
use crate::support::test_db;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_question_answered_while_its_session_is_busy_is_queued_behind_the_turn() {
    use gents_protocol::mailbox_question::{
        MailboxQuestion, MailboxQuestionAnswer, MailboxQuestionOption,
    };
    let db = test_db("mailbox-question-busy").await;
    let session_id = "mailbox-question-busy-session";
    let asking = "mailbox-question-busy-request";
    let prompt = "mailbox-question-busy-prompt";
    let question = MailboxQuestion {
        version: 1,
        prompt: "Keep going?".into(),
        options: ["yes", "no"]
            .map(|id| MailboxQuestionOption {
                id: id.into(),
                label: id.to_uppercase(),
                description: None,
                prompt: None,
            })
            .to_vec(),
        multi_select: false,
        allow_free_text: false,
        optional: None,
        recommended_order: None,
    };
    let reply = question
        .reply_content(
            "Continue",
            &MailboxQuestionAnswer {
                option_ids: vec!["yes".into()],
                free_text: None,
                skip: None,
                question_hash: None,
            },
        )
        .unwrap();
    let (prepared, turn) = crate::support::signed_mailbox::prepare_signed_mailbox_turn(
        &db,
        "mailbox-question-busy-backend",
        "mailbox-question-busy-model",
        asking,
        session_id,
        prompt,
        vec![StreamChunk::tool_call(
            "mailbox-question-busy-call",
            gents_documents::mailbox_write_decl::FILE_MAILBOX_ITEM_TOOL_NAME,
            serde_json::json!({"title":"Continue", "question":question}).to_string(),
        )],
        vec![StreamPlan::new(
            reply.clone(),
            vec![StreamResponse::completes(reply.clone(), ["noted"])],
        )],
        None,
    )
    .await
    .unwrap();
    prepared.backend.enable_dynamic_followups(prompt);
    let identity: Arc<dyn NodeIdentity> = db.node_identity.clone();
    let agent = gents::Gents::from_default_agent_documents(
        db.node.clone(),
        identity,
        gents::DocumentRuntimeOptions {
            backend_admission: Some(db.home.backend_admission()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let runtime = boot_prepared_accepted_turn(&db, prepared, agent).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let item = loop {
        let open = gents_session::operations::mailbox::list(
            &turn.client,
            &gents_protocol::operations::mailbox::MailboxListInput {
                requester_did: Some(turn.user.did().to_owned()),
                ..Default::default()
            },
        )
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
            crate::support::signed_mailbox::request_state_on(&turn.client, asking)
                .await
                .unwrap()
                .as_deref()
        )
    );

    let mut create = turn.request("mailbox-question-busy-reply", session_id, &reply);
    create.caused_by_source_doc_id = Some(item.doc_id.clone());
    create.input.queue = Some(gents_protocol::request_input::RequestQueue {
        delivery: Default::default(),
        position: None,
        source: gents_protocol::request_input::QueueSource::User,
        policy: gents_protocol::request_input::QueuePolicy::Append,
        key: None,
        queued_after_request_id: Some(asking.into()),
        interrupted_request_id: None,
        background_completion_wake_version: None,
    });
    create.input.question_reply = Some(
        gents_protocol::mailbox_question::MailboxQuestionReply::new(
            &item.doc_id,
            &item.title,
            item.summary.as_deref(),
            item.payload.as_deref().unwrap(),
            item.request_id.as_deref(),
            MailboxQuestionAnswer {
                option_ids: vec!["yes".into()],
                free_text: None,
                skip: None,
                question_hash: Some(
                    gents_protocol::mailbox_question::question_source_hash(
                        &item.title,
                        item.summary.as_deref(),
                        item.payload.as_deref().unwrap(),
                        item.request_id.as_deref(),
                    )
                    .unwrap(),
                ),
            },
        )
        .unwrap(),
    );
    let reply_doc = turn.submit(create).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        crate::support::signed_mailbox::request_state_on(
            &turn.client,
            "mailbox-question-busy-reply"
        )
        .await
        .unwrap()
        .as_deref(),
        Some("pending")
    );
    assert!(
        !gents_protocol::request_lifecycle::RequestLifecycleState::is_terminal_str(
            crate::support::signed_mailbox::request_state_on(&turn.client, asking)
                .await
                .unwrap()
                .as_deref()
        )
    );
    runtime
        .backend
        .enqueue_response(prompt, StreamResponse::completes(prompt, ["asking done"]));
    let asking_final = crate::support::signed_mailbox::wait_for_request_terminal_on(
        &turn.client,
        asking,
        Duration::from_secs(60),
    )
    .await
    .unwrap();
    let reply_final = crate::support::signed_mailbox::wait_for_request_terminal_on(
        &turn.client,
        "mailbox-question-busy-reply",
        Duration::from_secs(60),
    )
    .await
    .unwrap();
    assert_eq!(asking_final, "completed");
    assert_eq!(reply_final, "completed");
    let answered = gents_session::operations::mailbox::get(&turn.client, &item.doc_id)
        .await
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
