use gents_inference::llm::message::{Message, Text, ToolResult, ToolResultContent, UserContent};
use gents_session::session::*;
use gents_session::test_support::{fork_reader_node, history_on, session_rewrite_rows};
use gents_store::graphql::escape_graphql_string;
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
async fn retained_edit_fork_survives_reopen_without_copying_or_rechecking_current_agent() {
    use gents_store::identity::NodeIdentity as _;
    const SOURCE: &str = "retained-source";
    const CHILD: &str = "retained-child";
    let (mut db, user, client, binding) = fork_reader_node("retained-edit-fork").await;
    let node_did = db.node_identity.did().to_owned();
    let messages = (1..=3)
        .map(|sequence| format!("retained-{sequence}"))
        .collect::<Vec<_>>();
    let keys = publish_signed_history(&db, &user, &client, &binding, SOURCE, &messages).await;
    let params = ForkParams {
        source_session_id: SOURCE,
        boundary: ForkBoundary::BeforeMessageKey(&keys[2]),
        caller_node_did: &node_did,
        caller_requester_did: Some(user.did()),
        target_agent_did: None,
    };
    assert_eq!(
        fork_edit_on(&client, params.clone(), CHILD, user.did())
            .await
            .unwrap(),
        CHILD
    );
    let agent = escape_graphql_string(&binding.agent_did);
    let response = client
        .execute(&format!(
            r#"{{ Agent(filter: {{agent_did: {{_eq: "{agent}"}}}}, limit: 2) {{ _docID }} }}"#
        ))
        .await
        .unwrap();
    let [row] = response["data"]["Agent"].as_array().unwrap().as_slice() else {
        panic!("fixture Agent must be unique")
    };
    let doc = escape_graphql_string(row["_docID"].as_str().unwrap());
    let mutation = format!(
        r#"mutation($input: AgentMutationInputArg!) {{ update_Agent(docID: "{doc}", input: $input) {{ _docID }} }}"#
    );
    gents_store::config_client::ConfigAccess::local_as(
        db.node.clone(),
        identity::Did::new(&node_did).unwrap(),
    )
    .transact_write("test.disable_fork_agent", |txn| {
        let mutation = &mutation;
        Box::pin(async move {
            txn.execute_client_signed_update(
                mutation,
                &serde_json::json!({"input":{"enabled":false}}),
                gents_protocol::schemas::AGENT,
            )
            .await?;
            Ok::<_, anyhow::Error>(())
        })
    })
    .await
    .unwrap();
    let before = session_rewrite_rows(&client).await;
    let history = history_on(&client, CHILD, &node_did, Some(user.did()), None)
        .await
        .unwrap();
    assert_eq!(history.len(), 2);
    drop(client);
    db.simulate_process_crash().await.unwrap();
    let client = db.client_access(&user);
    assert_eq!(
        fork_edit_on(&client, params.clone(), CHILD, user.did())
            .await
            .unwrap(),
        CHILD
    );
    assert_eq!(session_rewrite_rows(&client).await, before);
    assert_eq!(
        history_on(&client, CHILD, &node_did, Some(user.did()), None)
            .await
            .unwrap(),
        history
    );
    let foreign = tokio::task::spawn_blocking(|| {
        gents_store::test_support::test_identity("retained-foreign")
    })
    .await
    .unwrap();
    for changed in [
        ForkParams {
            boundary: ForkBoundary::BeforeMessageKey(&keys[1]),
            ..params.clone()
        },
        ForkParams {
            source_session_id: "another-source",
            ..params.clone()
        },
        ForkParams {
            caller_requester_did: Some(foreign.did()),
            ..params.clone()
        },
    ] {
        assert!(fork_edit_on(&client, changed, CHILD, user.did())
            .await
            .is_err());
        assert_eq!(session_rewrite_rows(&client).await, before);
    }
    assert!(fork_edit_on(&client, params.clone(), CHILD, foreign.did())
        .await
        .is_err());
    assert_eq!(session_rewrite_rows(&client).await, before);
    client
        .transact_write("test.unbound_fork_session", |txn| {
            let binding = &binding;
            let node_did = &node_did;
            let user = &user;
            Box::pin(async move {
                ensure_session_in_txn(
                    txn,
                    "unbound-child",
                    node_did,
                    &binding.agent_did,
                    Some(user.did()),
                    None,
                    None,
                    "2026-09-22T00:00:00Z",
                )
                .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();
    let unbound_before = session_rewrite_rows(&client).await;
    assert!(fork_edit_on(&client, params, "unbound-child", user.did())
        .await
        .is_err());
    assert_eq!(session_rewrite_rows(&client).await, unbound_before);
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
        if case["running"].as_bool().unwrap() {
            let mode = if case["background"].as_bool().unwrap() {
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
        if case["published"].as_bool().unwrap() {
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
        if !case["same_requester"].as_bool().unwrap() {
            request.requester_did = Some(other.did().into());
        }
        if !case["same_session"].as_bool().unwrap() {
            request.session_id = format!("fresh-session-{index}");
        }
        if case["scoped_terminal"].as_bool().unwrap() {
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
                .unwrap_or_else(|error| panic!("{case}: {error:#}"));
        }
        let actual = gents_session::session::retry_has_published_input(node, &request).await;
        assert_eq!(actual.ok(), case["expected"].as_bool(), "{case}");
        if case["name"] == "ready" {
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
                case["expected"].as_bool(),
                "retry chain must find the same authored input"
            );
        }
    }
    node.shutdown().await.expect("shut down test node");
}
