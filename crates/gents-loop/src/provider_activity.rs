use std::sync::Mutex;

use tokio::time::Instant;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionIdentity {
    pub request: String,
    pub generation: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionPhase {
    Queued,
    Generating,
    ToolWait,
    ProviderRetry,
    Unavailable,
    Terminal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionObservation {
    pub identity: ExecutionIdentity,
    pub sequence: u64,
    pub turn: u64,
    pub attempt: u64,
    pub observed_at: u64,
    pub phase: ExecutionPhase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionDisplay {
    Unknown,
    Queued,
    Working,
    Waiting,
    Retrying,
    Unavailable,
    Terminal,
}

pub fn accept_execution_observation(
    active: &ExecutionIdentity,
    current: Option<ExecutionObservation>,
    incoming: ExecutionObservation,
) -> Option<ExecutionObservation> {
    if &incoming.identity != active {
        return current;
    }
    if current.as_ref().is_none_or(|previous| {
        &previous.identity != active
            || (incoming.sequence > previous.sequence
                && (incoming.turn > previous.turn
                    || (incoming.turn == previous.turn && incoming.attempt >= previous.attempt)))
    }) {
        Some(incoming)
    } else {
        current
    }
}

pub fn project_execution_activity(
    active: &ExecutionIdentity,
    terminal: bool,
    now: u64,
    max_age: u64,
    observation: Option<&ExecutionObservation>,
) -> ExecutionDisplay {
    if terminal {
        return ExecutionDisplay::Terminal;
    }
    let Some(observation) = observation else {
        return ExecutionDisplay::Unknown;
    };
    if &observation.identity != active
        || observation.observed_at > now
        || now - observation.observed_at > max_age
    {
        return ExecutionDisplay::Unknown;
    }
    match observation.phase {
        ExecutionPhase::Queued => ExecutionDisplay::Queued,
        ExecutionPhase::Generating => ExecutionDisplay::Working,
        ExecutionPhase::ToolWait => ExecutionDisplay::Waiting,
        ExecutionPhase::ProviderRetry => ExecutionDisplay::Retrying,
        ExecutionPhase::Unavailable => ExecutionDisplay::Unavailable,
        ExecutionPhase::Terminal => ExecutionDisplay::Unknown,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ActivityPhase {
    #[default]
    AwaitingSend,
    Active(Instant),
    Settled,
}

#[derive(Debug, Default)]
struct ActivityState {
    phase: ActivityPhase,
    stall_reason: Option<String>,
    token_limit_reached: bool,
}

#[derive(Debug, Default)]
pub struct ProviderActivity {
    state: Mutex<ActivityState>,
}

impl ProviderActivity {
    fn lock(&self) -> std::sync::MutexGuard<'_, ActivityState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn touch(&self) {
        self.lock().phase = ActivityPhase::Active(Instant::now());
    }

    pub fn settle(&self) {
        self.lock().phase = ActivityPhase::Settled;
    }

    pub fn phase(&self) -> ActivityPhase {
        self.lock().phase
    }

    pub(crate) fn observe_token_limit(&self) {
        self.lock().token_limit_reached = true;
    }

    pub(crate) fn token_limit_reached(&self) -> bool {
        self.lock().token_limit_reached
    }

    pub fn mark_stalled(&self, reason: String) {
        // The permit drop path reads this after the stalled stream is dropped.
        self.lock().stall_reason = Some(reason);
    }

    pub fn stall_reason(&self) -> Option<String> {
        self.lock().stall_reason.clone()
    }
}

#[cfg(test)]
mod execution_tests {
    use super::*;

    #[test]
    fn generated_execution_ordering_cases() {
        let raw = gents_lean_contract::run_prepared_generator(
            &gents_lean_contract::proofs_dir().unwrap(),
            "Proofs.Conformance.ExecutionActivityOrdering:olean",
            "Proofs/Conformance/ExecutionActivityOrdering.lean",
        )
        .unwrap();
        let cases: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap();
        assert_eq!(cases.len(), 108);
        let active = ExecutionIdentity {
            request: "request".into(),
            generation: "generation".into(),
        };
        let identity = |same: bool| ExecutionIdentity {
            request: "request".into(),
            generation: if same { "generation" } else { "other" }.into(),
        };
        for case in cases {
            let current = ExecutionObservation {
                identity: identity(case["currentSame"].as_bool().unwrap()),
                sequence: 2,
                turn: 1,
                attempt: 1,
                observed_at: 10,
                phase: ExecutionPhase::Generating,
            };
            let incoming = ExecutionObservation {
                identity: identity(case["incomingSame"].as_bool().unwrap()),
                sequence: case["sequence"].as_u64().unwrap(),
                turn: case["turn"].as_u64().unwrap(),
                attempt: case["attempt"].as_u64().unwrap(),
                observed_at: 11,
                phase: ExecutionPhase::ProviderRetry,
            };
            let result = accept_execution_observation(&active, Some(current), incoming.clone());
            assert_eq!(
                result == Some(incoming),
                case["accepted"].as_bool().unwrap(),
                "{case}"
            );
        }
    }

    #[test]
    fn generated_execution_projection_cases() {
        let raw = gents_lean_contract::run_prepared_generator(
            &gents_lean_contract::proofs_dir().unwrap(),
            "Proofs.Conformance.ExecutionActivity:olean",
            "Proofs/Conformance/ExecutionActivity.lean",
        )
        .unwrap();
        let cases: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap();
        assert_eq!(cases.len(), 96);
        let active = ExecutionIdentity {
            request: "request".into(),
            generation: "generation".into(),
        };
        for case in cases {
            let phase = match case["phase"].as_str().unwrap() {
                "queued" => ExecutionPhase::Queued,
                "generating" => ExecutionPhase::Generating,
                "tool_wait" => ExecutionPhase::ToolWait,
                "provider_retry" => ExecutionPhase::ProviderRetry,
                "unavailable" => ExecutionPhase::Unavailable,
                "terminal" => ExecutionPhase::Terminal,
                other => panic!("unknown modeled phase {other}"),
            };
            let observation = ExecutionObservation {
                identity: ExecutionIdentity {
                    request: "request".into(),
                    generation: if case["sameGeneration"] == true {
                        "generation"
                    } else {
                        "other"
                    }
                    .into(),
                },
                sequence: 2,
                turn: 0,
                attempt: 1,
                observed_at: case["observedAt"].as_u64().unwrap(),
                phase,
            };
            let display = project_execution_activity(
                &active,
                case["terminal"].as_bool().unwrap(),
                case["now"].as_u64().unwrap(),
                case["maxAge"].as_u64().unwrap(),
                Some(&observation),
            );
            let expected = match case["display"].as_str().unwrap() {
                "unknown" => ExecutionDisplay::Unknown,
                "queued" => ExecutionDisplay::Queued,
                "working" => ExecutionDisplay::Working,
                "waiting" => ExecutionDisplay::Waiting,
                "retrying" => ExecutionDisplay::Retrying,
                "unavailable" => ExecutionDisplay::Unavailable,
                "terminal" => ExecutionDisplay::Terminal,
                other => panic!("unknown modeled display {other}"),
            };
            assert_eq!(display, expected, "{case}");
        }
    }
}
