use std::sync::Arc;

use gents_desktop_core::client::ClientCore;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::{
    local::{LocalError, LocalListener},
    ClientHost,
};

#[derive(Debug, thiserror::Error)]
pub enum ClientHostServiceError {
    #[error("joining the client installation listener")]
    Join(#[source] tokio::task::JoinError),
    #[error(transparent)]
    Listener(#[from] LocalError),
}

/// The embedding shell must join attachments before shutting down their core.
/// Dropping a shutdown waiter must not abort submitted writes.
pub struct ClientHostService {
    stop: CancellationToken,
    task: Option<JoinHandle<Result<(), LocalError>>>,
}

impl ClientHostService {
    pub fn start(core: Arc<ClientCore>, listener: LocalListener) -> Self {
        Self::with_accounts(core, listener, Arc::default())
    }

    pub(crate) fn with_accounts(
        core: Arc<ClientCore>,
        listener: LocalListener,
        pending: Arc<crate::accounts::PendingOAuthCredentials>,
    ) -> Self {
        let stop = CancellationToken::new();
        let task = tokio::spawn(listener.serve(
            Arc::new(ClientHost::with_accounts(core, pending)),
            stop.clone(),
        ));
        Self {
            stop,
            task: Some(task),
        }
    }

    pub async fn shutdown(mut self) -> Result<(), ClientHostServiceError> {
        self.stop.cancel();
        Ok(self
            .task
            .take()
            .expect("client host shutdown owns its task")
            .await
            .map_err(ClientHostServiceError::Join)??)
    }
}

impl Drop for ClientHostService {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
