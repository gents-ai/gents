//! `MockModelEndpoint`: a thin wrapper over the shared robust [`FakeLlm`] fake
//! that only needs to answer the `/v1/models` discovery probe (optionally
//! bearer-gated). Public API is unchanged.

use std::sync::Arc;

use anyhow::Result;
use serde_json::Value;

use super::fake_llm::{ChatAction, FakeLlm};

pub struct MockModelEndpoint {
    inner: FakeLlm,
}

impl MockModelEndpoint {
    pub fn start(model_name: &str) -> Result<Self> {
        Self::start_with_required_bearer(model_name, None)
    }

    pub fn start_with_required_bearer(
        model_name: &str,
        required_bearer: Option<&str>,
    ) -> Result<Self> {
        // Discovery-only endpoint; chat is never driven through it, so the chat
        // responder is inert.
        let inner = FakeLlm::start(
            model_name,
            required_bearer,
            Arc::new(|_: &Value| ChatAction::Hang),
        )?;
        Ok(Self { inner })
    }

    pub fn endpoint(&self) -> &str {
        self.inner.endpoint()
    }
}
