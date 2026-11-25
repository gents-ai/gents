pub struct CountingRead {
    inner: gents_store::config_client::ConfigAccess,
    queries: std::sync::atomic::AtomicUsize,
}

impl CountingRead {
    pub fn new(inner: gents_store::config_client::ConfigAccess) -> Self {
        Self {
            inner,
            queries: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub fn queries(&self) -> usize {
        self.queries.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl gents_store::config_client::ConfigRead for CountingRead {
    fn reader_did(&self) -> Option<String> {
        self.inner.reader_did()
    }

    async fn execute_read(
        &self,
        document: &str,
    ) -> Result<serde_json::Value, gents_store::config_client::ConfigClientError> {
        self.queries
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.execute_read(document).await
    }

    async fn execute_read_with_variables(
        &self,
        document: &str,
        variables: &serde_json::Value,
    ) -> Result<serde_json::Value, gents_store::config_client::ConfigClientError> {
        self.queries
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner
            .execute_read_with_variables(document, variables)
            .await
    }
}

pub struct SessionRead {
    row: serde_json::Value,
    reads: std::sync::atomic::AtomicUsize,
}

impl SessionRead {
    pub fn new(row: serde_json::Value) -> Self {
        Self {
            row,
            reads: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub fn reads(&self) -> usize {
        self.reads.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl gents_store::config_client::ConfigRead for SessionRead {
    async fn execute_read(
        &self,
        query: &str,
    ) -> Result<serde_json::Value, gents_store::config_client::ConfigClientError> {
        assert!(
            query.contains("AgentSession") && !query.contains("AgentRequest"),
            "a starter is verified from its session row alone: {query}"
        );
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(serde_json::json!({"data": {"AgentSession": [self.row.clone()]}}))
    }
}

#[cfg(all(test, feature = "tauri-host"))]
#[derive(Default)]
pub(crate) struct FakeLaunch {
    pub(crate) approval: std::sync::Mutex<
        std::collections::VecDeque<Result<bool, gents_store::client_error::BridgeError>>,
    >,
    pub(crate) approval_blocks: bool,
    pub(crate) approval_gated: bool,
    pub(crate) approval_release: tokio::sync::Notify,
    pub(crate) starts: std::sync::Mutex<
        std::collections::VecDeque<
            Result<
                crate::tauri_commands::managed_server::launch::Launched,
                gents_store::client_error::BridgeError,
            >,
        >,
    >,
    pub(crate) readiness: std::sync::Mutex<
        std::collections::VecDeque<crate::tauri_commands::managed_server::readiness::Readiness>,
    >,
    pub(crate) start_calls: std::sync::atomic::AtomicUsize,
    pub(crate) approval_waits: std::sync::atomic::AtomicUsize,
    pub(crate) waiting: tokio::sync::Notify,
    pub(crate) still_booting: bool,
}

#[cfg(all(test, feature = "tauri-host"))]
impl crate::tauri_commands::managed_server::launch::ManagedLaunch for FakeLaunch {
    async fn requires_approval(&self) -> Result<bool, gents_store::client_error::BridgeError> {
        self.approval
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(false))
    }

    async fn await_approval(&self) -> anyhow::Result<()> {
        self.approval_waits
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.waiting.notify_one();
        if self.approval_blocks {
            std::future::pending::<()>().await;
        }
        if self.approval_gated {
            self.approval_release.notified().await;
        }
        Ok(())
    }

    async fn start(
        &self,
    ) -> Result<
        crate::tauri_commands::managed_server::launch::Launched,
        gents_store::client_error::BridgeError,
    > {
        self.start_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.starts.lock().unwrap().pop_front().unwrap_or(Ok(
            crate::tauri_commands::managed_server::launch::Launched::Started,
        ))
    }

    async fn await_ready(
        &self,
    ) -> Result<
        crate::tauri_commands::managed_server::readiness::Readiness,
        crate::tauri_commands::managed_server::readiness::StartError,
    > {
        let next = self.readiness.lock().unwrap().pop_front();
        match next {
            Some(readiness) => Ok(readiness),
            None if self.still_booting => Err(
                crate::tauri_commands::managed_server::readiness::RuntimeStillBooting {
                    waited: std::time::Duration::from_secs(300),
                }
                .into(),
            ),
            None => {
                self.waiting.notify_one();
                std::future::pending().await
            }
        }
    }
}

#[cfg(all(test, feature = "tauri-host"))]
impl FakeLaunch {
    pub(crate) fn starts(&self) -> usize {
        self.start_calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}
