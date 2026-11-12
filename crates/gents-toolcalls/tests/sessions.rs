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
        gents_session::test_support::no_lifecycle_plugins(),
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
