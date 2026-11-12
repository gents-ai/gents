use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct InferenceTarget {
    pub node: String,
    pub session: String,
    pub request: String,
    pub generation: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceActivity {
    AwaitingProvider,
    Working,
    Retrying,
    Waiting,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct InferenceObservation {
    pub target: InferenceTarget,
    pub turn: u64,
    pub attempt: u64,
    pub sequence: u64,
    pub observed_at: u64,
    pub activity: InferenceActivity,
}

/// A selection epoch fences reads started before a switch or reconnect. The
/// selected request and terminal flag come from the durable session owner.
#[derive(Default)]
pub struct InferenceObservationOwner {
    epoch: u64,
    target: Option<InferenceTarget>,
    terminal: bool,
    observation: Option<InferenceObservation>,
}

impl InferenceObservationOwner {
    pub fn select(&mut self, target: Option<InferenceTarget>, terminal: bool) -> u64 {
        self.epoch = self
            .epoch
            .checked_add(1)
            .expect("inference selection epoch exhausted");
        self.target = target;
        self.terminal = terminal;
        self.observation = None;
        self.epoch
    }

    pub fn receive(&mut self, epoch: u64, incoming: InferenceObservation) {
        let sequence = self.observation.as_ref().map_or(0, |o| o.sequence);
        let position = self
            .observation
            .as_ref()
            .map_or((0, 0), |o| (o.turn, o.attempt));
        if self.epoch == epoch
            && !self.terminal
            && self.target.as_ref() == Some(&incoming.target)
            && sequence < incoming.sequence
            && position <= (incoming.turn, incoming.attempt)
        {
            self.observation = Some(incoming);
        }
    }

    pub fn project(&self, now: u64, max_age: u64) -> Option<InferenceActivity> {
        if self.terminal {
            return None;
        }
        self.observation.as_ref().and_then(|observation| {
            now.checked_sub(observation.observed_at)
                .filter(|age| *age <= max_age)
                .map(|_| observation.activity)
        })
    }
}

impl super::ClientCore {
    pub(crate) fn begin_inference_observation(&self, target: &InferenceTarget) -> Option<u64> {
        let mut owners = self
            .inference_observations
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let key = (target.node.clone(), target.session.clone());
        if !owners.contains_key(&key) && owners.len() >= 256 {
            owners.clear();
        }
        let epoch = self
            .inference_observation_epoch
            .try_update(
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
                |value| value.checked_add(1),
            )
            .ok()?
            + 1;
        let owner = owners.entry(key).or_default();
        owner.select(Some(target.clone()), false);
        owner.epoch = epoch;
        Some(epoch)
    }

    pub(crate) fn finish_inference_observation(
        &self,
        epoch: u64,
        incoming: InferenceObservation,
        elapsed: u64,
    ) -> Option<InferenceActivity> {
        let mut owners = self
            .inference_observations
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let key = (
            incoming.target.node.clone(),
            incoming.target.session.clone(),
        );
        let owner = owners.get_mut(&key)?;
        if owner.epoch != epoch || owner.target.as_ref() != Some(&incoming.target) {
            return None;
        }
        owner.receive(epoch, incoming);
        owner.project(elapsed, 5_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(session: &str) -> InferenceTarget {
        InferenceTarget {
            node: "node".into(),
            session: session.into(),
            request: "request".into(),
            generation: "generation".into(),
        }
    }

    fn activity(name: &str) -> InferenceActivity {
        serde_json::from_value(serde_json::Value::String(name.to_owned())).unwrap()
    }

    #[test]
    fn generated_inference_observation_traces() {
        let cases = &gents_lean_vocab::lean_contract_snapshot().inference_observation_cases;
        assert!(!cases.is_empty());
        for case in cases {
            let mut owner = InferenceObservationOwner {
                target: Some(target("session")),
                ..Default::default()
            };
            for (action, expected) in case.actions.iter().zip(&case.expected) {
                let actual = match action.kind.as_str() {
                    "select" => {
                        owner.select(
                            action.target.as_ref().map(|target| InferenceTarget {
                                node: target.node.clone(),
                                session: target.session.clone(),
                                request: target.request.clone(),
                                generation: target.generation.clone(),
                            }),
                            action.terminal.unwrap(),
                        );
                        owner.observation.as_ref().map(|o| o.activity)
                    }
                    "receive" => {
                        let item = action.observation.as_ref().unwrap();
                        let incoming = InferenceObservation {
                            target: InferenceTarget {
                                node: item.target.node.clone(),
                                session: item.target.session.clone(),
                                request: item.target.request.clone(),
                                generation: item.target.generation.clone(),
                            },
                            turn: item.turn,
                            attempt: item.attempt,
                            sequence: item.sequence,
                            observed_at: item.observed_at,
                            activity: activity(&item.activity),
                        };
                        owner.receive(action.epoch.unwrap(), incoming);
                        owner.observation.as_ref().map(|o| o.activity)
                    }
                    "project" => owner.project(action.now.unwrap(), action.max_age.unwrap()),
                    _ => panic!("unknown generated action"),
                };
                assert_eq!(
                    serde_json::to_value(actual).unwrap(),
                    serde_json::to_value(expected).unwrap(),
                    "{case:?}"
                );
            }
        }
    }
}
