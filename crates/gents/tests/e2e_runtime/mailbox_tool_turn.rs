use std::sync::Arc;
use std::time::Duration;

use gents::session::mailbox::MailboxStatus;
use gents::NodeIdentity;

use crate::support::accepted_turn::boot_prepared_accepted_turn;
use crate::support::streaming_backend::{StreamChunk, StreamPlan, StreamResponse};
use crate::support::test_db;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn filing_a_mailbox_item_returns_a_receipt_and_the_turn_continues() {
    let db = test_db("mailbox-tool-turn").await;
    let did = db.node_identity.did().to_string();
    let request_id = "mailbox-tool-turn-request";
    let prompt = "mailbox-tool-turn-prompt";
    let (prepared, turn) = crate::support::signed_mailbox::prepare_signed_mailbox_turn(
        &db, "mailbox-tool-turn-backend", "mailbox-tool-turn-model", request_id,
        "mailbox-tool-turn-session", prompt,
        vec![StreamChunk::tool_call("mailbox-tool-turn-call", gents::session::mailbox::FILE_MAILBOX_ITEM_TOOL_NAME,
            serde_json::json!({"title":"Crew ready", "summary":"Two agents configured.\n\n- builder\n- reviewer"}).to_string())],
        Vec::new(), None,
    ).await.unwrap();
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

    let state = crate::support::signed_mailbox::wait_for_request_terminal_on(
        &turn.client,
        request_id,
        Duration::from_secs(60),
    )
    .await
    .unwrap();
    assert_eq!(state, "completed");
    let items = gents::session::mailbox::list_mailbox_items_on(
        &turn.client,
        turn.user.did(),
        Some(MailboxStatus::Open),
    )
    .await
    .unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].title, "Crew ready");
    assert_eq!(items[0].requester_did, turn.user.did());
    let commits = turn
        .agent_access
        .execute(&format!(
            r#"query {{_commits(docID: "{}") {{height fieldName signature {{identity type}}}}}}"#,
            gents::store::graphql::escape_graphql_string(&items[0].doc_id)
        ))
        .await
        .unwrap();
    let genesis = commits["data"]["_commits"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["fieldName"] == "_C")
        .min_by_key(|row| row["height"].as_i64())
        .unwrap();
    let expected_signer =
        gents::store::identity::commit_signer_identity_for_did(turn.agent.did()).unwrap();
    assert_eq!(genesis["signature"]["identity"], expected_signer);
    let outsider =
        gents::KeyIdentity::load_or_create(db.data_path().join("mailbox-outsider.key"), None)
            .unwrap();
    let admin = gents::ConfigAccess::local_as(db.node.clone(), did.parse().unwrap());
    for permission in [
        gents::store::config_client::NodePermission::DocumentRead,
        gents::store::config_client::NodePermission::CollectionGet,
    ] {
        admin
            .grant_node_permission(&outsider.did().parse().unwrap(), permission)
            .await
            .unwrap();
    }
    let outsider_access = gents::ConfigAccess::Graphql(
        gents::store::config_client::GraphqlEndpoint::as_principal(
            db.client_endpoint(),
            outsider.did(),
        )
        .with_client_signing_capability(outsider.client_signing_capability().unwrap())
        .unwrap(),
    );
    let hidden = outsider_access
        .execute(&format!(
            r#"{{MailboxItem(filter: {{_docID: {{_eq: "{}"}}}}) {{_docID}}}}"#,
            gents::store::graphql::escape_graphql_string(&items[0].doc_id)
        ))
        .await
        .unwrap();
    assert!(hidden["data"]["MailboxItem"].as_array().unwrap().is_empty());
    let outcome = serde_json::to_value(
        gents::session::mailbox::NotificationIdentity::Event.write_outcome(false, false),
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
    turn.client
        .transact_readonly("test.session.observed_reply", |txn| {
            let did = did.clone();
            let user = turn.user.did().to_owned();
            Box::pin(async move {
                let session = gents::session::load_agent_session_row_in_txn(
                    txn,
                    &did,
                    "mailbox-tool-turn-session",
                    Some(&user),
                )
                .await?
                .unwrap()
                .session;
                let observation = session.observation.unwrap();
                assert_eq!(observation.preview.as_deref(), Some(prompt));
                assert_eq!(
                    observation.reply_preview.as_deref(),
                    Some("parent complete")
                );
                assert_eq!(observation.latest_request.unwrap().request_id, request_id);
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();
    runtime.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_optional_question_skip_reaches_the_asking_session() {
    use gents_protocol::mailbox_question::{
        MailboxQuestion, MailboxQuestionAnswer, MailboxQuestionOption, MAILBOX_QUESTION_VERSION,
    };
    let db = test_db("mailbox-question-turn").await;
    let session_id = "mailbox-question-session";
    let question = MailboxQuestion {
        version: MAILBOX_QUESTION_VERSION,
        prompt: "Ship the release?".into(),
        options: ["yes", "no"]
            .map(|id| MailboxQuestionOption {
                id: id.into(),
                label: id.to_uppercase(),
                description: None,
                prompt: None,
            })
            .to_vec(),
        multi_select: false,
        allow_free_text: true,
        optional: Some(true),
        recommended_order: None,
    };
    let mut answer = MailboxQuestionAnswer {
        skip: Some(true),
        ..Default::default()
    };
    let reply = question.reply_content("Release", &answer).unwrap();
    let (prepared, turn) = crate::support::signed_mailbox::prepare_signed_mailbox_turn(
        &db,
        "mailbox-question-backend",
        "mailbox-question-model",
        "mailbox-question-request",
        session_id,
        "mailbox-question-prompt",
        vec![StreamChunk::tool_call(
            "mailbox-question-call",
            gents::session::mailbox::FILE_MAILBOX_ITEM_TOOL_NAME,
            serde_json::json!({"title":"Release", "question":question}).to_string(),
        )],
        vec![StreamPlan::new(
            reply.clone(),
            vec![StreamResponse::completes(reply.clone(), ["noted"])],
        )],
        None,
    )
    .await
    .unwrap();
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
    assert_eq!(
        crate::support::signed_mailbox::wait_for_request_terminal_on(
            &turn.client,
            "mailbox-question-request",
            Duration::from_secs(60)
        )
        .await
        .unwrap(),
        "completed"
    );
    let items = gents::session::mailbox::list_mailbox_items_on(
        &turn.client,
        turn.user.did(),
        Some(MailboxStatus::Open),
    )
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

    let mut create = turn.request("mailbox-question-reply", session_id, &reply);
    create.caused_by_source_doc_id = Some(item.doc_id.clone());
    answer.question_hash = Some(
        gents_protocol::mailbox_question::question_source_hash(
            &item.title,
            item.summary.as_deref(),
            item.payload.as_deref().unwrap(),
            item.request_id.as_deref(),
        )
        .unwrap(),
    );
    create.input.question_reply = Some(
        gents_protocol::mailbox_question::MailboxQuestionReply::new(
            &item.doc_id,
            &item.title,
            item.summary.as_deref(),
            item.payload.as_deref().unwrap(),
            item.request_id.as_deref(),
            answer,
        )
        .unwrap(),
    );
    let reply_doc = turn.submit(create).await.unwrap();
    assert_eq!(
        crate::support::signed_mailbox::wait_for_request_terminal_on(
            &turn.client,
            "mailbox-question-reply",
            Duration::from_secs(60)
        )
        .await
        .unwrap(),
        "completed"
    );
    let answered =
        gents::session::mailbox::load_mailbox_item_with_access(&turn.client, &item.doc_id)
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
        delivered.contains("Skipped optional decision on Release."),
        "the answer must reach the model in the asking session: {delivered}"
    );
    runtime.shutdown().await;
}

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
            gents::session::mailbox::FILE_MAILBOX_ITEM_TOOL_NAME,
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
        let open = gents::session::mailbox::list_mailbox_items_on(
            &turn.client,
            turn.user.did(),
            Some(MailboxStatus::Open),
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
    let answered =
        gents::session::mailbox::load_mailbox_item_with_access(&turn.client, &item.doc_id)
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
