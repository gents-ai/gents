use std::sync::{Arc, Mutex};
use std::time::Duration;

use gents_loop::provider_activity::ActivityPhase;
use gents_loop::session_hook::ProviderObservation;
use rapidhash::RapidHashMap;
use serde::{Deserialize, Serialize};

pub const INFERENCE_ACTIVITY_PATH: &str = "/inference/activity";

const MAX_ACTIVE_OBSERVATIONS: usize = 256;
const SILENT_AFTER: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
pub struct InferenceKey {
    pub node: String,
    pub session: String,
    pub request: String,
    pub generation: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceActivity {
    AwaitingProvider,
    Waiting,
    Working,
    Retrying,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct InferenceSnapshot {
    pub node: String,
    pub session: String,
    pub request: String,
    pub generation: String,
    pub turn: usize,
    pub attempt: u32,
    pub sequence: u64,
    pub activity: InferenceActivity,
}

struct Entry {
    turn: usize,
    attempt: u32,
    sequence: u64,
    observation: Option<ProviderObservation>,
    tools: RapidHashMap<String, std::time::Instant>,
}

#[derive(Clone, Default)]
pub struct InferenceObservationRegistry(Arc<Mutex<RapidHashMap<InferenceKey, Entry>>>);

pub struct InferenceObservationGuard {
    registry: InferenceObservationRegistry,
    key: InferenceKey,
}

impl InferenceObservationRegistry {
    pub fn begin(&self, key: InferenceKey) -> Option<InferenceObservationGuard> {
        let mut entries = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if entries.len() >= MAX_ACTIVE_OBSERVATIONS || entries.contains_key(&key) {
            return None;
        }
        entries.insert(
            key.clone(),
            Entry {
                turn: 0,
                attempt: 0,
                sequence: 0,
                observation: None,
                tools: Default::default(),
            },
        );
        Some(InferenceObservationGuard {
            registry: self.clone(),
            key,
        })
    }

    pub fn snapshot(&self, key: &InferenceKey) -> Option<InferenceSnapshot> {
        let mut entries = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let entry = entries.get_mut(key)?;
        let activity = if !entry.tools.is_empty() {
            if entry
                .tools
                .values()
                .any(|started| started.elapsed() < SILENT_AFTER)
            {
                InferenceActivity::Working
            } else {
                InferenceActivity::Waiting
            }
        } else {
            match entry.observation.as_ref() {
                None => InferenceActivity::AwaitingProvider,
                Some(ProviderObservation::Retrying { .. }) => InferenceActivity::Retrying,
                Some(ProviderObservation::Attempt { activity, .. }) => {
                    match activity.as_ref().map(|activity| activity.phase()) {
                        Some(ActivityPhase::Active(last)) if last.elapsed() < SILENT_AFTER => {
                            InferenceActivity::Working
                        }
                        _ => InferenceActivity::Waiting,
                    }
                }
            }
        };
        entry.sequence = entry.sequence.checked_add(1)?;
        Some(InferenceSnapshot {
            node: key.node.clone(),
            session: key.session.clone(),
            request: key.request.clone(),
            generation: key.generation.clone(),
            turn: entry.turn,
            attempt: entry.attempt,
            sequence: entry.sequence,
            activity,
        })
    }
}

pub struct ToolObservationGuard {
    owner: std::sync::Weak<InferenceObservationGuard>,
    id: String,
}

impl Drop for ToolObservationGuard {
    fn drop(&mut self) {
        let Some(owner) = self.owner.upgrade() else {
            return;
        };
        let mut entries = owner.registry.0.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(entry) = entries.get_mut(&owner.key) {
            entry.tools.remove(&self.id);
        }
    }
}

impl InferenceObservationGuard {
    pub fn begin_tool(self: &Arc<Self>, id: &str) -> ToolObservationGuard {
        let mut entries = self.registry.0.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(entry) = entries.get_mut(&self.key) {
            entry.tools.insert(id.to_owned(), std::time::Instant::now());
        }
        ToolObservationGuard {
            owner: Arc::downgrade(self),
            id: id.to_owned(),
        }
    }

    pub fn observe(&self, observation: ProviderObservation) {
        let mut entries = self.registry.0.lock().unwrap_or_else(|p| p.into_inner());
        let Some(entry) = entries.get_mut(&self.key) else {
            return;
        };
        let (turn, attempt) = match &observation {
            ProviderObservation::Attempt { turn, attempt, .. }
            | ProviderObservation::Retrying { turn, attempt } => (*turn, *attempt),
        };
        if (turn, attempt) >= (entry.turn, entry.attempt) {
            entry.turn = turn;
            entry.attempt = attempt;
            entry.observation = Some(observation);
        }
    }
}

impl Drop for InferenceObservationGuard {
    fn drop(&mut self) {
        self.registry
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.key);
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> InferenceKey {
        InferenceKey {
            node: "node".into(),
            session: "session".into(),
            request: "request".into(),
            generation: "generation".into(),
        }
    }

    #[test]
    fn retry_and_turn_observations_are_fenced_and_removed_on_drop() {
        let registry = InferenceObservationRegistry::default();
        let guard = registry.begin(key()).unwrap();
        let activity = Arc::new(gents_loop::provider_activity::ProviderActivity::default());
        activity.touch();
        guard.observe(ProviderObservation::Attempt {
            turn: 0,
            attempt: 0,
            activity: Some(activity),
        });
        let first = registry.snapshot(&key()).unwrap();
        assert!(matches!(first.activity, InferenceActivity::Working));
        guard.observe(ProviderObservation::Retrying {
            turn: 0,
            attempt: 0,
        });
        let retry = registry.snapshot(&key()).unwrap();
        assert!(matches!(retry.activity, InferenceActivity::Retrying));
        assert!(retry.sequence > first.sequence);
        guard.observe(ProviderObservation::Attempt {
            turn: 1,
            attempt: 0,
            activity: None,
        });
        guard.observe(ProviderObservation::Retrying {
            turn: 0,
            attempt: 4,
        });
        let next = registry.snapshot(&key()).unwrap();
        assert_eq!((next.turn, next.attempt), (1, 0));
        assert!(matches!(next.activity, InferenceActivity::Waiting));
        drop(guard);
        assert!(registry.snapshot(&key()).is_none());
    }

    fn activity_of(registry: &InferenceObservationRegistry) -> Option<String> {
        registry
            .snapshot(&key())
            .and_then(|snapshot| serde_json::to_value(snapshot.activity).ok())
            .and_then(|value| value.as_str().map(str::to_owned))
    }

    fn observe_run(
        run: &gents_lean_vocab::families::inference_observation_registry_activity::Run<String>,
    ) -> gents_lean_vocab::families::inference_observation_registry_activity::Outcome<String> {
        let registry = InferenceObservationRegistry::default();
        let mut requests: RapidHashMap<u64, Arc<InferenceObservationGuard>> =
            RapidHashMap::default();
        let mut tools: RapidHashMap<(u64, String), ToolObservationGuard> = RapidHashMap::default();
        let mut activities = Vec::with_capacity(run.steps.len());
        for step in &run.steps {
            match step.kind.as_str() {
                "open_request" => {
                    if let Some(guard) = registry.begin(key()) {
                        requests.insert(step.request, Arc::new(guard));
                    }
                }
                "close_request" => {
                    requests.remove(&step.request);
                }
                "open_tool" => {
                    if let Some(request) = requests.get(&step.request) {
                        let guard = request.begin_tool(&step.tool);
                        tools.insert((step.request, step.tool.clone()), guard);
                    }
                }
                "close_tool" => {
                    tools.remove(&(step.request, step.tool.clone()));
                }
                other => unreachable!("unknown step kind {other}"),
            }
            activities.push(activity_of(&registry));
        }
        gents_lean_vocab::families::inference_observation_registry_activity::Outcome { activities }
    }

    #[test]
    fn observation_registry_matches_the_lean_model() {
        gents_lean_vocab::families::inference_observation_registry_activity::replay::<String, String>(
            observe_run,
        );
    }
}
