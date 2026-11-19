use std::sync::{Arc, OnceLock};

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
