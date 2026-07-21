//! `MockChatEndpoint`: a thin wrapper over the shared robust [`FakeLlm`] fake
//! that replies to chat completions with a fixed behavior (text completion,
//! routed-by-content-with-delay, or hang). Public API is unchanged.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;

use super::fake_llm::{completion_text_sse, ChatAction, FakeLlm};

pub struct MockChatEndpoint {
    inner: FakeLlm,
}

impl MockChatEndpoint {
    pub fn start(model_name: &str, final_text: &str) -> Result<Self> {
        Self::start_with_required_bearer(model_name, final_text, None)
    }

    pub fn start_hanging(model_name: &str) -> Result<Self> {
        let inner = FakeLlm::start(model_name, None, Arc::new(|_: &Value| ChatAction::Hang))?;
        Ok(Self { inner })
    }

    pub fn start_routed_delayed(
        model_name: &str,
        routes: Vec<(String, String)>,
        default_text: String,
        delay: Duration,
    ) -> Result<Self> {
        let responder = Arc::new(move |request: &Value| {
            let text =
                routed_completion_text(request, &routes).unwrap_or_else(|| default_text.clone());
            ChatAction::DelayThenSse(delay, completion_text_sse(&text))
        });
        let inner = FakeLlm::start(model_name, None, responder)?;
        Ok(Self { inner })
    }

    pub fn start_with_required_bearer(
        model_name: &str,
        final_text: &str,
        required_bearer: Option<&str>,
    ) -> Result<Self> {
        let final_text = final_text.to_string();
        let responder =
            Arc::new(move |_: &Value| ChatAction::Sse(completion_text_sse(&final_text)));
        let inner = FakeLlm::start(model_name, required_bearer, responder)?;
        Ok(Self { inner })
    }

    pub fn endpoint(&self) -> &str {
        self.inner.endpoint()
    }

    pub fn captured_chat_requests(&self) -> Vec<Value> {
        self.inner.captured_chat_requests()
    }
}

fn routed_completion_text(request_json: &Value, routes: &[(String, String)]) -> Option<String> {
    let request = request_json.to_string();
    routes
        .iter()
        .find(|(needle, _)| request.contains(needle))
        .map(|(_, response)| response.clone())
}
