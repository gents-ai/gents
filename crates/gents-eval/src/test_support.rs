pub(crate) mod doubles;
pub(crate) use gents::eval::test_support::{completion, draft, identity, origin, HeldEvalFixture};
pub(crate) use gents_documents::test_support::bootstrap_signed_test_node;
pub(crate) use gents_store::test_support::runtime_handle;

pub(crate) struct SubscribedObservationHook {
    pub(crate) ready: tokio::sync::Notify,
    notified: std::sync::atomic::AtomicBool,
}

impl SubscribedObservationHook {
    pub(crate) fn new() -> Self {
        Self {
            ready: tokio::sync::Notify::new(),
            notified: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

#[async_trait::async_trait]
impl crate::runner::embedded::ObservationHook for SubscribedObservationHook {
    async fn on_state(&self, _state: &str, _session_id: Option<&str>) {
        if !self
            .notified
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            self.ready.notify_one();
        }
    }
}
