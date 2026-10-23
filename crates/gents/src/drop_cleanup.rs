//! Asynchronous cleanup owed by a guard's synchronous `Drop`.

use std::future::Future;

/// The runtime a guard was created on, to which its synchronous `Drop` hands
/// asynchronous cleanup.
///
/// `Drop` may run on a thread without a Tokio context (an unwinding or aborted
/// task, a runtime shutting down), so a guard binds its runtime when it is
/// created. Cleanup handed over after that runtime shut down is dropped
/// without running.
#[derive(Clone, Debug)]
pub struct DropCleanup(tokio::runtime::Handle);

impl DropCleanup {
    /// Binds the runtime of the calling task; panics outside a Tokio runtime.
    #[allow(
        clippy::disallowed_methods,
        reason = "a drop guard binds the runtime of the task that creates it, because its Drop may run where no runtime is current"
    )]
    pub fn current() -> Self {
        Self(tokio::runtime::Handle::current())
    }

    pub fn spawn<F>(&self, cleanup: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        drop(self.0.spawn(cleanup));
    }
}
