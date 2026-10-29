use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;

use super::errors::invalid_params;
use crate::commands::grok_shim::projection::AsyncCommit;

#[derive(Debug, Clone)]
pub(super) struct AcpSessionState {
    pub(super) mode_id: String,
    pub(super) resume_from: Option<String>,
}

impl AcpSessionState {
    pub(super) fn new() -> Self {
        Self {
            mode_id: "default".to_string(),
            resume_from: None,
        }
    }
}

/// Connection-local reservation only. No mutex is held across replay I/O;
/// cancellation releases the reservation synchronously through Drop.
pub(super) struct SessionLoadReservation<'a> {
    loading: &'a std::sync::Mutex<BTreeSet<String>>,
    session_id: String,
}

impl<'a> SessionLoadReservation<'a> {
    pub(super) fn reserve(
        loading: &'a std::sync::Mutex<BTreeSet<String>>,
        session_id: &str,
    ) -> Result<Self> {
        if !loading
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(session_id.to_owned())
        {
            return Err(invalid_params(
                "session is already loading on this connection",
            ));
        }
        Ok(Self {
            loading,
            session_id: session_id.to_owned(),
        })
    }
}

impl Drop for SessionLoadReservation<'_> {
    fn drop(&mut self) {
        self.loading
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&self.session_id);
    }
}

pub(super) struct ModeStateCommit<'a> {
    pub(super) sessions: &'a tokio::sync::Mutex<BTreeMap<String, AcpSessionState>>,
    pub(super) session_id: String,
    pub(super) mode_id: String,
}

impl AsyncCommit for ModeStateCommit<'_> {
    async fn commit(&self) {
        let mut sessions = self.sessions.lock().await;
        if let Some(state) = sessions.get_mut(&self.session_id) {
            state.mode_id = self.mode_id.clone();
        }
    }
}
