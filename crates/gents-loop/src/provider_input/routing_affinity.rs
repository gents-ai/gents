use std::future::Future;
use std::sync::{Arc, OnceLock};

/// Codex routing state belongs to one main inference execution. The shared
/// backend client must never retain it; server tokens are opaque and ephemeral.
pub struct RoutingAffinity {
    session: String,
    token: OnceLock<String>,
}

impl RoutingAffinity {
    pub fn start(codex: bool, main: bool, session: Option<String>) -> Option<Arc<Self>> {
        (codex && main).then_some(session).flatten().map(|session| {
            Arc::new(Self {
                session,
                token: OnceLock::new(),
            })
        })
    }

    pub fn session(&self) -> &str {
        &self.session
    }
    pub fn token(&self) -> Option<&str> {
        self.token.get().map(String::as_str)
    }
    pub fn observe(&self, token: Option<&str>) {
        if let Some(token) = token {
            let _ = self.token.set(token.to_owned());
        }
    }
}

tokio::task_local! {
    static ROUTING_AFFINITY: Option<Arc<RoutingAffinity>>;
}

pub fn current() -> Option<Arc<RoutingAffinity>> {
    ROUTING_AFFINITY.try_with(Clone::clone).ok().flatten()
}

pub async fn scope<F: Future>(state: Option<Arc<RoutingAffinity>>, future: F) -> F::Output {
    ROUTING_AFFINITY.scope(state, future).await
}
