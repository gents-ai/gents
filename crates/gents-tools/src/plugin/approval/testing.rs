use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result};

use super::Answer;
use gents_store::drop_cleanup::DropCleanup;

enum ObserveWake {
    Fs,
    Stop,
}

/// The simulated external operator stays ready while a guest occupies its test
/// task. Its answer wait starts when awaited, after guest preparation. Cleanup
/// stops the worker and hands its join to the runtime bound at construction.
pub struct Operator {
    stop: mpsc::Sender<ObserveWake>,
    worker: Option<tokio::task::JoinHandle<Result<ObservedQuestion>>>,
    cleanup: DropCleanup,
    wait: Duration,
    observation: QuestionObservation,
}

pub(crate) struct ObservedQuestion {
    id: String,
    prompt: String,
}

#[derive(Clone)]
pub(crate) struct QuestionObservation(Arc<Mutex<Option<String>>>);

impl QuestionObservation {
    #[cfg(test)]
    pub(crate) fn id(&self) -> Option<String> {
        self.0.lock().expect("approval observation lock").clone()
    }
}

impl Operator {
    pub async fn answer(home: &Path, answer: Answer) -> Result<Self> {
        Self::with_wait(home, answer, Duration::from_secs(10))
            .await
            .map(|(operator, _)| operator)
    }

    #[cfg(test)]
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
        let (tx, rx) = mpsc::channel();
        let fs_tx = tx.clone();
        let worker = tokio::task::spawn_blocking(move || observe(home, answer, rx, fs_tx));
        let observation = QuestionObservation(Arc::new(Mutex::new(None)));
        Ok((
            Self {
                stop: tx,
                worker: Some(worker),
                cleanup: DropCleanup::current(),
                wait,
                observation: observation.clone(),
            },
            observation,
        ))
    }
}

/// Wait until a pending approval request appears on `home`, waking on
/// filesystem events in the approval queue directories.
pub async fn wait_for_question(
    home: &Path,
    advertise_listener: bool,
    ceiling: Duration,
    what: &str,
) -> super::Request {
    use notify::Watcher as _;
    super::prepare_listener(home).expect("prepare approval queue watch");
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut watcher = notify::recommended_watcher(move |event| {
        let _ = tx.send(event);
    })
    .expect("create approval queue watcher");
    for dir in super::queue_dirs(home) {
        watcher
            .watch(&dir, notify::RecursiveMode::NonRecursive)
            .expect("watch approval queue directory");
    }
    tokio::time::timeout(ceiling, async {
        loop {
            let mut requests = if advertise_listener {
                super::pending(home)
            } else {
                super::pending_requests(home)
            }
            .expect("read pending approval requests");
            if let Some(request) = requests.pop() {
                return request;
            }
            if rx.recv().await.is_none() {
                panic!("{what}: approval queue watcher closed");
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what}"))
}

fn observe(
    home: PathBuf,
    answer: Answer,
    rx: mpsc::Receiver<ObserveWake>,
    fs_tx: mpsc::Sender<ObserveWake>,
) -> Result<ObservedQuestion> {
    use notify::Watcher as _;
    let mut watcher = notify::recommended_watcher(move |_| {
        let _ = fs_tx.send(ObserveWake::Fs);
    })?;
    for dir in super::queue_dirs(&home) {
        watcher.watch(&dir, notify::RecursiveMode::NonRecursive)?;
    }
    loop {
        if let Some(request) = super::pending(&home)?.into_iter().next() {
            let id = request.id.clone();
            let prompt = request.prompt();
            super::decide(&home, &id, answer)?;
            return Ok(ObservedQuestion { id, prompt });
        }
        match rx.recv_timeout(super::LISTENER_FRESH / 2) {
            Ok(ObserveWake::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                anyhow::bail!("the test operator was stopped before answering");
            }
            Ok(ObserveWake::Fs) | Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

impl std::future::IntoFuture for Operator {
    type Output = Result<String>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send>>;

    fn into_future(mut self) -> Self::IntoFuture {
        Box::pin(async move {
            let reply = tokio::time::timeout(
                self.wait,
                self.worker
                    .as_mut()
                    .expect("operator awaited after completion"),
            )
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "the test operator observed no approval question before its wait ended"
                )
            })?
            .context("the test approval operator stopped without a result")??;
            self.worker.take();
            *self
                .observation
                .0
                .lock()
                .expect("approval observation lock") = Some(reply.id);
            Ok(reply.prompt)
        })
    }
}

impl Drop for Operator {
    fn drop(&mut self) {
        let _ = self.stop.send(ObserveWake::Stop);
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
