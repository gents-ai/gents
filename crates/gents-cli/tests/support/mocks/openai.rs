use std::sync::Arc;

use anyhow::Result;
use serde_json::Value;

use super::fake_llm::{completion_text_sse, tool_call_sse, ChatAction, FakeLlm};
use super::request_has_tool_result_message;

pub struct MockOpenAIEndpoint {
    inner: FakeLlm,
}

impl MockOpenAIEndpoint {
    pub fn start(model_name: &str, final_token: &str) -> Result<Self> {
        let final_token = final_token.to_string();
        let responder = Arc::new(move |request: &Value| {
            if request_has_tool_result_message(request) {
                ChatAction::Sse(completion_text_sse(&final_token))
            } else {
                ChatAction::Sse(tool_call_sse("read_file", r#"{"path":"notes.txt"}"#))
            }
        });
        let inner = FakeLlm::start(model_name, None, responder)?;
        Ok(Self { inner })
    }

    pub fn endpoint(&self) -> &str {
        self.inner.endpoint()
    }

    pub fn captured_chat_requests(&self) -> Vec<Value> {
        self.inner.captured_chat_requests()
    }
}
