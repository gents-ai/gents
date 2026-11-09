use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::task::{Context, Poll};
use std::time::Duration;

use anyhow::{Context as _, Result};

use super::Answer;
use crate::drop_cleanup::DropCleanup;

/// The simulated external operator stays ready while a guest occupies its test
/// task. Its answer wait starts when polled, after guest preparation. Cleanup
/// stops the worker and hands its join to the runtime bound at construction.
pub(crate) struct Operator {
    stop: Arc<AtomicBool>,
    worker: Option<tokio::task::JoinHandle<Result<ObservedQuestion>>>,
    cleanup: DropCleanup,
    wait: Duration,
    deadline: Option<Pin<Box<tokio::time::Sleep>>>,
    observation: QuestionObservation,
}

pub(crate) struct ObservedQuestion {
    id: String,
    prompt: String,
}

#[derive(Clone)]
pub(crate) struct QuestionObservation(Arc<Mutex<Option<String>>>);

impl QuestionObservation {
    pub(crate) fn id(&self) -> Option<String> {
        self.0.lock().expect("approval observation lock").clone()
    }
}

impl Operator {
    pub(crate) async fn answer(home: &Path, answer: Answer) -> Result<Self> {
        Self::with_wait(home, answer, Duration::from_secs(10))
            .await
            .map(|(operator, _)| operator)
    }

    pub(crate) async fn answer_with_observation(
        home: &Path,
        answer: Answer,
    ) -> Result<(Self, QuestionObservation)> {
        Self::with_wait(home, answer, Duration::from_secs(10)).await
    }

    async fn with_wait(
        home: &Path,
        answer: Answer,
        wait: Duration,
    ) -> Result<(Self, QuestionObservation)> {
        let home = home.to_owned();
        let ready_home = home.clone();
        tokio::task::spawn_blocking(move || super::mark_listening(&ready_home))
            .await
            .context("preparing the test approval operator")??;
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let worker = tokio::task::spawn_blocking(move || observe(home, answer, &stopping));
        let observation = QuestionObservation(Arc::new(Mutex::new(None)));
        Ok((
            Self {
                stop,
                worker: Some(worker),
                cleanup: DropCleanup::current(),
                wait,
                deadline: None,
                observation: observation.clone(),
            },
            observation,
        ))
    }
}

/// The file-based queue has no asynchronous receiver. Its external operator
/// polls on a blocking worker so guest scheduling cannot stop the heartbeat.
#[expect(
    clippy::disallowed_methods,
    reason = "the test operator owns a spawn_blocking worker while polling the native filesystem queue"
)]
fn observe(home: PathBuf, answer: Answer, stop: &AtomicBool) -> Result<ObservedQuestion> {
    while !stop.load(Ordering::Acquire) {
        if let Some(request) = super::pending(&home)?.into_iter().next() {
            let id = request.id.clone();
            let prompt = request.prompt();
            super::decide(&home, &id, answer)?;
            return Ok(ObservedQuestion { id, prompt });
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    anyhow::bail!("the test operator was stopped before answering")
}

impl Future for Operator {
    type Output = Result<String>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let worker = this
            .worker
            .as_mut()
            .expect("operator polled after completion");
        if let Poll::Ready(reply) = Pin::new(worker).poll(cx) {
            this.worker.take();
            let question =
                reply.context("the test approval operator stopped without a result")??;
            *this
                .observation
                .0
                .lock()
                .expect("approval observation lock") = Some(question.id);
            return Poll::Ready(Ok(question.prompt));
        }
        let deadline = this
            .deadline
            .get_or_insert_with(|| Box::pin(tokio::time::sleep(this.wait)));
        if deadline.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(anyhow::anyhow!(
                "the test operator observed no approval question before its wait ended"
            )));
        }
        Poll::Pending
    }
}

impl Drop for Operator {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.abort();
            self.cleanup.spawn(async move {
                let _ = worker.await;
            });
        }
    }
}

#[tokio::test]
async fn an_operator_advertises_both_native_queues_before_its_future_is_polled() {
    let home = tempfile::tempdir().unwrap();
    let operator = Operator::answer(home.path(), Answer::Once).await.unwrap();
    for queue in super::Queue::ALL {
        assert!(super::listener_fresh(home.path(), queue), "{queue:?}");
    }
    drop(operator);
    assert!(super::pending_requests(home.path()).unwrap().is_empty());
}

#[tokio::test]
async fn an_unanswered_operator_settles_instead_of_leaving_a_detached_waiter() {
    let home = tempfile::tempdir().unwrap();
    let (operator, _) = Operator::with_wait(home.path(), Answer::Once, Duration::ZERO)
        .await
        .unwrap();
    let error = operator.await.unwrap_err();
    assert!(
        error.to_string().contains("observed no approval question"),
        "{error:#}"
    );
}
