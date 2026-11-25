use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Result;

use crate::auth::credential::BearerSource;

pub struct StaticBearer {
    token: std::result::Result<String, String>,
    calls: AtomicUsize,
    invalidations: AtomicUsize,
}

impl StaticBearer {
    pub fn new(token: &str) -> Self {
        Self {
            token: Ok(token.to_string()),
            calls: Default::default(),
            invalidations: Default::default(),
        }
    }

    pub fn failing(message: &str) -> Self {
        Self {
            token: Err(message.to_string()),
            calls: Default::default(),
            invalidations: Default::default(),
        }
    }

    pub fn bearer_calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    pub fn invalidations(&self) -> usize {
        self.invalidations.load(Ordering::SeqCst)
    }
}

impl BearerSource for StaticBearer {
    async fn current_bearer(&self) -> Result<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.token
            .clone()
            .map_err(|message| anyhow::anyhow!(message))
    }

    async fn invalidate(&self) {
        self.invalidations.fetch_add(1, Ordering::SeqCst);
    }
}
