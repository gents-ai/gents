use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value;

use crate::support::commit_race_proxy::{CommitRaceProxy, ProxyHook, WriteCheckpoint};

#[derive(Deserialize)]
pub struct Step {
    pub action: String,
    pub actor: Option<u64>,
    pub before: Value,
    pub after: Value,
}

#[derive(Deserialize)]
pub struct Trace {
    spec: String,
    pub config: String,
    expect: String,
    pub constants: Value,
    pub steps: Vec<Step>,
}

pub enum Red {
    Defect,
    Premise,
    Witness,
}

impl Trace {
    pub fn check(&self, red: Red, properties: &[(&str, bool)]) {
        let violated = self
            .expect
            .strip_prefix("violates ")
            .expect("counterexample");
        assert!(
            properties.iter().any(|(name, _)| *name == violated),
            "{} has no observation for {violated}",
            self.config
        );
        for &(property, holds) in properties {
            let expected = property != violated || matches!(red, Red::Defect);
            assert_eq!(holds, expected, "{}: {property}", self.config);
        }
    }
}

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
        let trace: Trace = serde_json::from_str(json).expect("TLC trace");
        assert_eq!(trace.spec, self.spec);
        for &(name, expected) in self.constants {
            assert_eq!(
                trace.constants[name], expected,
                "{} constant {name}",
                trace.config
            );
        }
        trace
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
