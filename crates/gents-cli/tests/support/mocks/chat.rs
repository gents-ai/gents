use std::sync::Arc;

use anyhow::Result;
use serde_json::Value;

use gents::support::fake_llm::{completion_text_sse, ChatAction, FakeLlm};

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
}
