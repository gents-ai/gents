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
    use serde_json::Value;

    fn target(session: &str) -> InferenceTarget {
        InferenceTarget {
            node: "node".into(),
            session: session.into(),
            request: "request".into(),
            generation: "generation".into(),
        }
    }

    #[test]
    fn generated_inference_observation_traces() {
        let snapshot: Value = gents_lean_contract::load_contract_snapshot().unwrap();
        let cases = snapshot["inference_observation_cases"].as_array().unwrap();
        assert!(!cases.is_empty());
        for case in cases {
            let mut owner = InferenceObservationOwner {
                target: Some(target("session")),
                ..Default::default()
            };
            for (action, expected) in case["actions"]
                .as_array()
                .unwrap()
                .iter()
                .zip(case["expected"].as_array().unwrap())
            {
                let actual = match action["kind"].as_str().unwrap() {
                    "select" => {
                        owner.select(
                            serde_json::from_value(action["target"].clone()).unwrap(),
                            action["terminal"].as_bool().unwrap(),
                        );
                        owner.observation.as_ref().map(|o| o.activity)
                    }
                    "receive" => {
                        let item = &action["observation"];
                        let incoming = InferenceObservation {
                            target: serde_json::from_value(item["target"].clone()).unwrap(),
                            turn: item["turn"].as_u64().unwrap(),
                            attempt: item["attempt"].as_u64().unwrap(),
                            sequence: item["sequence"].as_u64().unwrap(),
                            observed_at: item["observed_at"].as_u64().unwrap(),
                            activity: serde_json::from_value(item["activity"].clone()).unwrap(),
                        };
                        owner.receive(action["epoch"].as_u64().unwrap(), incoming);
                        owner.observation.as_ref().map(|o| o.activity)
                    }
                    "project" => owner.project(
                        action["now"].as_u64().unwrap(),
                        action["max_age"].as_u64().unwrap(),
                    ),
                    _ => panic!("unknown generated action"),
                };
                assert_eq!(serde_json::to_value(actual).unwrap(), *expected, "{case}");
            }
        }
    }
}
