use std::sync::Arc;

use anyhow::{Context, Result};
use gents_desktop_core::client::ClientCore;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::{local::LocalListener, ClientHost};

/// The embedding shell must join attachments before shutting down their core.
/// Dropping a shutdown waiter must not abort submitted writes.
pub struct ClientHostService {
    stop: CancellationToken,
    task: Option<JoinHandle<Result<()>>>,
}

impl ClientHostService {
    pub fn start(core: Arc<ClientCore>, listener: LocalListener) -> Self {
        let stop = CancellationToken::new();
        let task = tokio::spawn(listener.serve(Arc::new(ClientHost::new(core)), stop.clone()));
        Self {
            stop,
            task: Some(task),
        }
    }

    pub async fn shutdown(mut self) -> Result<()> {
        self.stop.cancel();
        self.task
            .take()
            .expect("client host shutdown owns its task")
            .await
            .context("joining the client installation listener")?
    }
}

impl Drop for ClientHostService {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
