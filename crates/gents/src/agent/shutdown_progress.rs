use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::watch;

use crate::behavior_readiness_publisher::BehaviorAdmissionObservation;

/// What one agent run is still awaiting, so a caller whose shutdown bound
/// expires can name the outstanding owner instead of only the elapsed time.
///
/// Diagnostic only: nothing reads it to gate, order or cancel work. The
/// process lifecycle phase is read from the readiness publisher that owns it;
/// this type records only what no other owner does: the background tasks the
/// shutdown owner has not yet joined, which await the enrollment reconciler is
/// in, and which requests are inside `process_request`. Labels are static
/// names or identifiers already carried by the request row, never payload text.
#[derive(Clone, Default)]
pub struct RuntimeShutdownProgress {
    state: Arc<Mutex<ProgressState>>,
}

#[derive(Default)]
struct ProgressState {
    process_state: Option<watch::Receiver<BehaviorAdmissionObservation>>,
    tasks: BTreeMap<&'static str, usize>,
    enrollment: Option<&'static str>,
    requests: BTreeMap<(String, String), usize>,
}

impl RuntimeShutdownProgress {
    fn state(&self) -> MutexGuard<'_, ProgressState> {
        // A panic while holding this lock cannot leave a diagnostic label in a
        // state that matters to correctness.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) fn observe_process_state(
        &self,
        observation: watch::Receiver<BehaviorAdmissionObservation>,
    ) {
        self.state().process_state = Some(observation);
    }

    /// Count `name` as pending from now until the returned future completes or
    /// is dropped, including by abort or panic.
    pub(crate) fn track_task<F: Future>(
        &self,
        name: &'static str,
        task: F,
    ) -> impl Future<Output = F::Output> {
        *self.state().tasks.entry(name).or_default() += 1;
        let pending = PendingTask {
            progress: self.clone(),
            name,
        };
        async move {
            let _pending = pending;
            task.await
        }
    }

    pub(crate) fn enrollment_await(&self, awaiting: &'static str) -> EnrollmentAwait {
        self.state().enrollment = Some(awaiting);
        EnrollmentAwait {
            progress: self.clone(),
        }
    }

    /// Held for one whole request; the request's own span covers its items.
    pub(crate) fn process_request(&self, behavior_id: &str, request_id: &str) -> ActiveRequest {
        let key = (behavior_id.to_owned(), request_id.to_owned());
        *self.state().requests.entry(key.clone()).or_default() += 1;
        ActiveRequest {
            progress: self.clone(),
            key,
        }
    }
}

impl fmt::Display for RuntimeShutdownProgress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state();
        match &state.process_state {
            Some(observation) => write!(
                f,
                "process_state={:?}",
                observation.borrow().process_state()
            )?,
            None => write!(f, "process_state=<not initialized>")?,
        }
        let tasks: Vec<_> = state
            .tasks
            .iter()
            .map(|(name, count)| match count {
                1 => (*name).to_owned(),
                count => format!("{name}x{count}"),
            })
            .collect();
        write!(f, "; unjoined background tasks=[{}]", tasks.join(", "))?;
        write!(
            f,
            "; enrollment reconciler awaiting={}",
            state.enrollment.unwrap_or("<idle>")
        )?;
        let requests: Vec<_> = state
            .requests
            .keys()
            .map(|(behavior_id, request_id)| format!("{behavior_id}/{request_id}"))
            .collect();
        write!(f, "; requests in process_request=[{}]", requests.join(", "))
    }
}

struct PendingTask {
    progress: RuntimeShutdownProgress,
    name: &'static str,
}

impl Drop for PendingTask {
    fn drop(&mut self) {
        let mut state = self.progress.state();
        if let Some(count) = state.tasks.get_mut(self.name) {
            *count -= 1;
            if *count == 0 {
                state.tasks.remove(self.name);
            }
        }
    }
}

pub(crate) struct EnrollmentAwait {
    progress: RuntimeShutdownProgress,
}

impl Drop for EnrollmentAwait {
    fn drop(&mut self) {
        self.progress.state().enrollment = None;
    }
}

pub(crate) struct ActiveRequest {
    progress: RuntimeShutdownProgress,
    key: (String, String),
}

impl Drop for ActiveRequest {
    fn drop(&mut self) {
        let mut state = self.progress.state();
        if let Some(count) = state.requests.get_mut(&self.key) {
            *count -= 1;
            if *count == 0 {
                state.requests.remove(&self.key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn names_only_what_is_still_outstanding() {
        let progress = RuntimeShutdownProgress::default();
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let pending = tokio::spawn(progress.track_task("enrollment_reconcile", async move {
            let _ = released.await;
        }));
        progress.track_task("router", async {}).await;
        let request = progress.process_request("behavior-a", "request-1");
        let enrollment = progress.enrollment_await("periodic sweep");
        assert_eq!(
            progress.to_string(),
            "process_state=<not initialized>; unjoined background tasks=[enrollment_reconcile]; \
             enrollment reconciler awaiting=periodic sweep; \
             requests in process_request=[behavior-a/request-1]"
        );

        drop(enrollment);
        drop(request);
        release.send(()).unwrap();
        pending.await.unwrap();
        assert_eq!(
            progress.to_string(),
            "process_state=<not initialized>; unjoined background tasks=[]; \
             enrollment reconciler awaiting=<idle>; requests in process_request=[]"
        );
    }

    #[tokio::test]
    async fn an_aborted_task_is_no_longer_pending() {
        let progress = RuntimeShutdownProgress::default();
        let task = tokio::spawn(progress.track_task("stuck", std::future::pending::<()>()));
        assert!(progress.to_string().contains("tasks=[stuck]"));
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(progress.to_string().contains("tasks=[]"));
    }
}
