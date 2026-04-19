use std::sync::Arc;
use std::time::Duration;

use rig::agent::{HookAction, PromptHook};
use rig::completion::message::{AssistantContent, Message, Reasoning, Text, UserContent};
use rig::completion::{CompletionError, CompletionModel, CompletionRequest, CompletionResponse};
use rig::one_or_many::OneOrMany;
use rig::streaming::StreamingCompletionResponse;

use super::*;
use crate::ensure_schemas;
use crate::hook::FailurePolicy;
use crate::lifecycle::RequestLifecycle;
use crate::watcher::AgentRequest;

#[derive(Clone, Default)]
struct TestModel;

#[allow(refining_impl_trait)]
impl CompletionModel for TestModel {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_: &Self::Client, _: impl Into<String>) -> Self {
        Self
    }

    async fn completion(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        Err(CompletionError::ProviderError(
            "completion is unused in stream processor tests".to_string(),
        ))
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
        Err(CompletionError::ProviderError(
            "streaming is unused in stream processor tests".to_string(),
        ))
    }
}

fn user_text_message(text: &str) -> Message {
    Message::User {
        content: OneOrMany::one(UserContent::Text(Text {
            text: text.to_string(),
        })),
    }
}

#[tokio::test]
async fn persist_partial_turn_saves_reasoning_and_text_to_history() {
    let data_path =
        std::env::temp_dir().join(format!("agent-stream-processor-{}", uuid::Uuid::new_v4()));
    let node = Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(&data_path)
            .build()
            .await
            .unwrap(),
    );
    ensure_schemas(&node).await.unwrap();

    let hook = DefraSessionHook::with_identity(
        node.clone(),
        "general",
        "did:defra-agent:test",
        FailurePolicy::default(),
    );
    assert!(matches!(
        PromptHook::<TestModel>::on_completion_call(
            &hook,
            &user_text_message("Inspect the repo"),
            &[]
        )
        .await,
        HookAction::Continue
    ));

    let session_id = hook.session_id().await.expect("session id");
    let request = AgentRequest {
        doc_id: "request-doc".to_string(),
        request_id: uuid::Uuid::new_v4().to_string(),
        agent_did: "did:defra-agent:test".to_string(),
        behavior_id: Some("general".to_string()),
        session_id: session_id.clone(),
        content: "Inspect the repo".to_string(),
        temperature: None,
        top_p: None,
        top_k: None,
        max_tokens: None,
        metadata: None,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        "test-agent",
        "did:defra-agent:test",
        request,
        30,
        crate::lifecycle::ExecutionOrigin::Interactive,
        "test-backend",
    );
    let stream_writer = crate::streaming::DefraStreamWriter::new(
        node.clone(),
        "did:defra-agent:test",
        Duration::from_secs(60),
    );
    let mut processor = StreamProcessor::new(&hook, &stream_writer, &mut lifecycle, "response-doc");

    processor.assistant_turn.push_reasoning(
        Reasoning::new("Need to inspect directory structure first")
            .with_id("rs_partial".to_string()),
    );
    processor
        .assistant_turn
        .push_text("I started by checking the repo layout.");

    assert!(processor.has_observable_activity());
    assert!(processor
        .persist_partial_turn("persist errored assistant turn")
        .await
        .unwrap());

    let history = crate::session::load_history(&node, &session_id)
        .await
        .unwrap();
    assert_eq!(history.len(), 2);
    assert!(matches!(
        &history[1],
        Message::Assistant { content, .. }
            if content.len() == 2
                && matches!(content.first_ref(), AssistantContent::Reasoning(reasoning)
                    if reasoning.id.as_deref() == Some("rs_partial"))
                && matches!(content.iter().nth(1), Some(AssistantContent::Text(Text { text }))
                    if text == "I started by checking the repo layout.")
    ));

    let _ = std::fs::remove_dir_all(&data_path);
}
