use gents_inference::llm::message::{Message, Text, ToolResult, ToolResultContent, UserContent};
use gents_session::session::*;
use gents_session::test_support::history_on;
use gents_store::test_support::first_content;

#[tokio::test]
async fn provider_history_excludes_current_input_but_keeps_its_tool_results() {
    use gents_protocol::tool_call::AwaitMode;
    use gents_store::identity::NodeIdentity;
    use gents_toolcalls::call::lifecycle::admission_fixture::publish_accepted_on_claimed_request;
    let (db, user, client, binding) = gents_inference::test_support::signed_test_node(
        "provider-history",
        gents_automation::lifecycle_hooks::composition(),
    )
    .await;
    let session_id = "session-steering-provider-history";
    publish_signed_history(
        &db,
        &user,
        &client,
        &binding,
        session_id,
        &["older steering".into()],
    )
    .await;
    let mut current = signed_claimed_authored_request(
        &db,
        &user,
        &client,
        &binding,
        "current-steering",
        session_id,
    )
    .await;
    let mut tool = publish_accepted_on_claimed_request(
        db.node.clone(),
        &mut current,
        db.node_identity.did(),
        0,
        "fixture",
        "call-current",
        serde_json::json!({}),
        AwaitMode::Foreground,
        true,
    )
    .await
    .unwrap();
    let writer = gents_session::request::streaming::DefraStreamWriter::new_for_lifecycle(
        db.node.clone(),
        &current,
        std::time::Duration::ZERO,
    )
    .unwrap();
    writer
        .publish_authored_message(&current, "prompt", &Message::user("current steering"))
        .await
        .unwrap();
    tool.complete("tool finished").await.unwrap();
    let history = history_on(
        &client,
        session_id,
        db.node_identity.did(),
        Some(user.did()),
        Some(&current.request().doc_id),
    )
    .await
    .unwrap();
    assert_eq!(history.len(), 3);
    assert!(matches!(history.first(), Some(Message::User {content})
        if matches!(first_content(content), UserContent::Text(Text {text}) if text == "older steering")));
    assert!(
        matches!(&history[1], Message::Assistant {content, ..} if content.iter().any(|block|
        matches!(block, gents_inference::llm::message::AssistantContent::ToolCall(call) if call.id == "call-current")))
    );
    let tool_result = Message::User {
        content: vec![UserContent::ToolResult(ToolResult {
            id: "call-current".into(),
            call_id: Some("fixture-provider-call-0".into()),
            content: vec![ToolResultContent::Text(Text {
                text: "tool finished".into(),
            })],
        })],
    };
    assert_eq!(history.get(2), Some(&tool_result));
    db.node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn retry_frontier_matches_lean_admission() {
    use gents_protocol::tool_call::AwaitMode;
    use gents_store::identity::NodeIdentity;
    use gents_toolcalls::call::lifecycle::admission_fixture::publish_accepted_on_claimed_request;
    let (db, user, client, binding) = gents_inference::test_support::signed_test_node(
        "retry-frontier",
        gents_automation::lifecycle_hooks::composition(),
    )
    .await;
    let node = &db.node;
    let node_did = db.node_identity.did();
    let other = tokio::task::spawn_blocking(|| {
        gents_store::test_support::test_identity("retry-foreign-user")
    })
    .await
    .unwrap();
    for (index, case) in gents_lean_vocab::lean_contract_snapshot()
        .retry_frontier_cases
        .iter()
        .enumerate()
    {
        let session_id = format!("retry-frontier-{index}");
        let parent_request_id = format!("retry-frontier-parent-{index}");
        let mut parent = signed_claimed_authored_request(
            &db,
            &user,
            &client,
            &binding,
            &parent_request_id,
            &session_id,
        )
        .await;
        let writer = gents_session::request::streaming::DefraStreamWriter::new_for_lifecycle(
            node.clone(),
            &parent,
            std::time::Duration::ZERO,
        )
        .unwrap();
        if case.running {
            let mode = if case.background {
                AwaitMode::Background
            } else {
                AwaitMode::Foreground
            };
            let _tool = publish_accepted_on_claimed_request(
                node.clone(),
                &mut parent,
                node_did,
                0,
                "fixture",
                &format!("retry-tool-{index}"),
                serde_json::json!({}),
                mode,
                true,
            )
            .await
            .unwrap();
        } else {
            parent.begin_owned_execution(&writer).await.unwrap();
        }
        if case.published {
            writer
                .publish_authored_message(&parent, "prompt", &Message::user("Do the work"))
                .await
                .unwrap();
        }
        let parent_doc = parent.request().doc_id.clone();
        let mut request = parent.request().clone();
        request.doc_id = "successor".into();
        request.request_id = "successor".into();
        request.retry_parent_request_doc_id = Some(parent_doc.clone());
        if !case.same_requester {
            request.requester_did = Some(other.did().into());
        }
        if !case.same_session {
            request.session_id = format!("fresh-session-{index}");
        }
        if case.scoped_terminal {
            // A running parent already published its tool-call assistant
            // header, which the terminal must select rather than discard.
            let scope = parent.request().clone();
            let headers = parent
                .access()
                .unwrap()
                .transact_readonly("test.retry_frontier.headers", |txn| {
                    let scope = &scope;
                    Box::pin(async move {
                        load_request_headers_in_txn(
                            txn,
                            &scope.session_id,
                            &scope.node_did,
                            scope.requester_did.as_deref(),
                            &scope.doc_id,
                        )
                        .await
                    })
                })
                .await
                .unwrap();
            let selection = headers
                .iter()
                .filter(|header| {
                    header.message.role == gents_protocol::output::MessageRole::Assistant
                })
                .max_by_key(|header| header.message.sequence)
                .map_or(
                    gents_protocol::output::TerminalOutput::NoMessage,
                    |header| gents_protocol::output::TerminalOutput::Message {
                        message_doc_id: header.doc_id.clone(),
                    },
                );
            parent
                .terminalize_owned(
                    gents_session::request::lifecycle::RequestTerminalOutcome::Dead,
                    selection,
                    Some("Stale"),
                )
                .await
                .unwrap_or_else(|error| panic!("{case:?}: {error:#}"));
        }
        let actual = gents_session::session::retry_has_published_input(node, &request).await;
        assert_eq!(actual.ok(), case.expected, "{case:?}");
        if case.name == "ready" {
            let retry = gents_session::request::lifecycle::RetryLink {
                parent_request_id: Some(parent_request_id.clone()),
                parent_request_doc_id: Some(parent_doc.clone()),
                root_request_id: parent_request_id.clone(),
                retry_count: 1,
                max_retries: 3,
            };
            let mut second = signed_claimed_authored_request_with_retry(
                &db,
                &user,
                &client,
                &binding,
                "retry-frontier-second",
                &session_id,
                Some(retry),
            )
            .await;
            let second_writer =
                gents_session::request::streaming::DefraStreamWriter::new_for_lifecycle(
                    node.clone(),
                    &second,
                    std::time::Duration::ZERO,
                )
                .unwrap();
            second.begin_owned_execution(&second_writer).await.unwrap();
            second
                .terminalize_owned(
                    gents_session::request::lifecycle::RequestTerminalOutcome::Failed,
                    gents_protocol::output::TerminalOutput::NoMessage,
                    None,
                )
                .await
                .unwrap();
            request.retry_parent_request_doc_id = Some(second.request().doc_id.clone());
            assert_eq!(
                gents_session::session::retry_has_published_input(node, &request)
                    .await
                    .ok(),
                case.expected,
                "retry chain must find the same authored input"
            );
        }
    }
    node.shutdown().await.expect("shut down test node");
}
