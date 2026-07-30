use std::sync::Arc;

use async_trait::async_trait;

use gents::{AgentIdentity, ServiceAccount};

#[allow(dead_code)]
pub(crate) struct StubAgentIdentity {
    pub did: String,
}

impl StubAgentIdentity {
    #[allow(dead_code)]
    pub(crate) fn new(did: impl Into<String>) -> Self {
        Self { did: did.into() }
    }

    #[allow(dead_code)]
    pub(crate) fn arc(did: impl Into<String>) -> Arc<dyn AgentIdentity> {
        Arc::new(Self::new(did))
    }
}

#[async_trait]
impl AgentIdentity for StubAgentIdentity {
    fn did(&self) -> &str {
        &self.did
    }

    async fn sign(&self, _payload: &[u8]) -> anyhow::Result<Vec<u8>> {
        panic!(
            "StubAgentIdentity::sign called for {} — routing tests must not sign",
            self.did
        )
    }

    async fn verify(&self, _did: &str, _payload: &[u8], _sig: &[u8]) -> anyhow::Result<bool> {
        panic!(
            "StubAgentIdentity::verify called for {} — routing tests must not verify",
            self.did
        )
    }

    fn service_account(&self) -> Option<&ServiceAccount> {
        None
    }
}
