use std::sync::Mutex;

use tokio::time::Instant;

pub use gents_protocol::execution_activity::{
    accept_execution_observation, project_execution_activity, ExecutionDisplay, ExecutionIdentity,
    ExecutionObservation, ExecutionPhase,
};

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
    fn generated_request_execution_projection_cases() {
        let raw = gents_lean_contract::run_prepared_generator(
            &gents_lean_contract::proofs_dir().unwrap(),
            "Proofs.Conformance.ExecutionActivityRequest:olean",
            "Proofs/Conformance/ExecutionActivityRequest.lean",
        )
        .unwrap();
        let cases: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap();
        assert_eq!(cases.len(), 8);
        let observation = ExecutionObservation {
            identity: ExecutionIdentity {
                request: "request".into(),
                generation: "generation".into(),
            },
            sequence: 0,
            turn: 0,
            attempt: 0,
            observed_at: 10,
            phase: ExecutionPhase::ProviderRetry,
        };
        for case in cases {
            use gents_protocol::request_lifecycle::RequestLifecycleState as State;
            let request = gents_protocol::row::AgentRequestRow {
                doc_id: Some("request".into()),
                execution_generation: case["hasGeneration"]
                    .as_bool()
                    .unwrap()
                    .then(|| "generation".into()),
                lifecycle_state: Some(if case["terminal"] == true {
                    State::Completed
                } else if case["pending"] == true {
                    State::Pending
                } else {
                    State::Processing
                }),
                ..Default::default()
            };
            let actual = gents_protocol::execution_activity::project_request_execution_activity(
                &request,
                10,
                1,
                Some(&observation),
            );
            let expected = match case["display"].as_str().unwrap() {
                "terminal" => ExecutionDisplay::Terminal,
                "queued" => ExecutionDisplay::Queued,
                "retrying" => ExecutionDisplay::Retrying,
                "unknown" => ExecutionDisplay::Unknown,
                other => panic!("unknown modeled display {other}"),
            };
            assert_eq!(actual, expected, "{case}");
        }
    }

    #[test]
    fn generated_execution_failure_cases() {
        let raw = gents_lean_contract::run_prepared_generator(
            &gents_lean_contract::proofs_dir().unwrap(),
            "Proofs.Conformance.ExecutionActivityFailure:olean",
            "Proofs/Conformance/ExecutionActivityFailure.lean",
        )
        .unwrap();
        let cases: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap();
        assert_eq!(cases.len(), 4);
        for case in cases {
            let phase = gents_protocol::execution_activity::failure_execution_phase(
                case["retry"].as_bool().unwrap(),
                case["unreachable"].as_bool().unwrap(),
            );
            let expected = match case["phase"].as_str().unwrap() {
                "provider_retry" => ExecutionPhase::ProviderRetry,
                "unavailable" => ExecutionPhase::Unavailable,
                "unknown" => ExecutionPhase::Unknown,
                other => panic!("unknown modeled phase {other}"),
            };
            assert_eq!(phase, expected, "{case}");
        }
    }

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
        assert_eq!(cases.len(), 112);
        let active = ExecutionIdentity {
            request: "request".into(),
            generation: "generation".into(),
        };
        for case in cases {
            let phase = match case["phase"].as_str().unwrap() {
                "unknown" => ExecutionPhase::Unknown,
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
