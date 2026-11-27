use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use gents_lean_vocab::tla::replay::{Step, Trace};

use crate::support::commit_race_proxy::{CommitRaceProxy, ProxyHook, WriteCheckpoint};

pub enum Action<E> {
    Environment(E),
    Commit,
    Internal,
}

pub struct SpecTraceMap<E> {
    pub spec: &'static str,
    pub constants: &'static [(&'static str, u32)],
    pub action: fn(&Step) -> Action<E>,
}

pub struct CommitReplay {
    pub proxy: CommitRaceProxy,
    commits: Arc<AtomicUsize>,
    scheduled: usize,
}

impl CommitReplay {
    pub fn commits(&self) -> usize {
        let count = self.commits.load(Ordering::SeqCst);
        assert!(
            count >= self.scheduled,
            "reached {count} of {} checkpoints",
            self.scheduled
        );
        count
    }
}

impl<E> SpecTraceMap<E>
where
    E: Clone + Send + Sync + 'static,
{
    pub fn load(&self, json: &str) -> Trace {
        Trace::load(json, self.spec, self.constants)
    }

    pub async fn drive<F, Fut>(
        &self,
        trace: &Trace,
        upstream: &str,
        before_first_begin: Option<E>,
        perform: F,
    ) -> CommitReplay
    where
        F: Fn(E, usize) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let mut checkpoints = Vec::new();
        let mut pending = Vec::new();
        for step in &trace.steps {
            match (self.action)(step) {
                Action::Environment(action) => pending.push(action),
                Action::Commit => checkpoints.push(std::mem::take(&mut pending)),
                Action::Internal => {}
            }
        }
        assert!(
            pending.is_empty(),
            "{} has trailing environment actions",
            trace.config
        );
        let scheduled = checkpoints.len();
        let commits = Arc::new(AtomicUsize::new(0));
        let hook: ProxyHook = {
            let commits = commits.clone();
            let checkpoints = Arc::new(checkpoints);
            let perform = Arc::new(perform);
            let before_first_begin = std::sync::Mutex::new(before_first_begin);
            Arc::new(move |checkpoint| {
                let (commits, checkpoints, perform) =
                    (commits.clone(), checkpoints.clone(), perform.clone());
                let begin = if checkpoint == WriteCheckpoint::Begin {
                    before_first_begin.lock().unwrap().take()
                } else {
                    None
                };
                Box::pin(async move {
                    if let Some(action) = begin {
                        perform(action, 0).await;
                    }
                    if checkpoint == WriteCheckpoint::Commit {
                        let index = commits.fetch_add(1, Ordering::SeqCst);
                        if let Some(actions) = checkpoints.get(index) {
                            for action in actions {
                                perform(action.clone(), index).await;
                            }
                        }
                    }
                })
            })
        };
        CommitReplay {
            proxy: CommitRaceProxy::spawn(upstream, hook).await,
            commits,
            scheduled,
        }
    }
}
