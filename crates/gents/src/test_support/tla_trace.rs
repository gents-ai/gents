use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value;

use crate::support::commit_race_proxy::{is_commit, CommitRaceProxy, ProxyHook};

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
    pub fn load(spec: &str, json: &str) -> Self {
        let trace: Self = serde_json::from_str(json).expect("TLC trace");
        assert_eq!(trace.spec, spec);
        assert_eq!(
            trace.constants["Attempts"],
            gents_store::config_client::TRANSACT_CONFLICT_MAX_RETRIES + 1,
            "{} attempt bound",
            trace.config
        );
        trace
    }

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
    pub async fn drive<F, Fut>(&self, trace: &Trace, upstream: &str, perform: F) -> CommitReplay
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
            Arc::new(move |method, path| {
                let (commits, checkpoints, perform) =
                    (commits.clone(), checkpoints.clone(), perform.clone());
                Box::pin(async move {
                    if is_commit(&method, &path) {
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
