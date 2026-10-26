#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Accepting,
    Stopping,
    Released,
}

/// Counts come from existing execution and native cleanup owners under the
/// same admission fence. They do not replace durable request lifecycle state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct State {
    pub phase: Phase,
    pub executions: usize,
    pub native_outstanding: usize,
}

/// The node DID owner authenticates the caller and binds its command to the
/// observed running build. Paths, PIDs and version ordering are insufficient.
#[derive(Clone, Copy, Debug)]
pub struct RetirementAuthority {
    pub operator_authenticated: bool,
    pub observed_build_matches: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum Action {
    Admit,
    Finish,
    NativeStarted,
    NativeReturned,
    Retire(RetirementAuthority),
    Release { store_released: bool },
    Replace { startup_reserved: bool },
}

impl State {
    pub fn step(self, action: Action) -> Option<Self> {
        let mut next = self;
        match action {
            Action::Admit if self.phase == Phase::Accepting => {
                next.executions = self.executions.checked_add(1)?;
            }
            Action::Finish if self.phase != Phase::Released && self.executions > 0 => {
                next.executions -= 1;
            }
            Action::NativeStarted if self.phase != Phase::Released => {
                next.native_outstanding = self.native_outstanding.checked_add(1)?;
            }
            Action::NativeReturned if self.native_outstanding > 0 => {
                next.native_outstanding -= 1;
            }
            Action::Retire(authority)
                if self.phase == Phase::Accepting
                    && self.executions == 0
                    && authority.operator_authenticated
                    && authority.observed_build_matches =>
            {
                next.phase = Phase::Stopping;
            }
            Action::Release {
                store_released: true,
            } if self.phase == Phase::Stopping
                && self.executions == 0
                && self.native_outstanding == 0 =>
            {
                next.phase = Phase::Released;
            }
            Action::Replace {
                startup_reserved: true,
            } if self.phase == Phase::Released
                && self.executions == 0
                && self.native_outstanding == 0 =>
            {
                next.phase = Phase::Accepting;
            }
            _ => return None,
        }
        Some(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn state(value: &Value) -> State {
        State {
            phase: match value["phase"].as_str().unwrap() {
                "accepting" => Phase::Accepting,
                "stopping" => Phase::Stopping,
                "released" => Phase::Released,
                other => panic!("unknown modeled phase {other}"),
            },
            executions: value["executions"].as_u64().unwrap().try_into().unwrap(),
            native_outstanding: value["nativeOutstanding"]
                .as_u64()
                .unwrap()
                .try_into()
                .unwrap(),
        }
    }

    #[test]
    fn generated_replacement_traces_match_authenticated_admission_and_release_guards() {
        let snapshot: Value = gents_lean_contract::load_contract_snapshot().unwrap();
        let cases = snapshot["runtime_service_replacement_cases"]
            .as_array()
            .unwrap();
        assert!(!cases.is_empty());
        for case in cases {
            let mut current = state(&case["initial"]);
            for step in case["trace"].as_array().unwrap() {
                let raw = &step["action"];
                let action = match raw["kind"].as_str().unwrap() {
                    "admit" => Action::Admit,
                    "finish" => Action::Finish,
                    "nativeStarted" => Action::NativeStarted,
                    "nativeReturned" => Action::NativeReturned,
                    "retire" => Action::Retire(RetirementAuthority {
                        operator_authenticated: raw["operatorAuthenticated"].as_bool().unwrap(),
                        observed_build_matches: raw["observedBuildMatches"].as_bool().unwrap(),
                    }),
                    "release" => Action::Release {
                        store_released: raw["storeReleased"].as_bool().unwrap(),
                    },
                    "replace" => Action::Replace {
                        startup_reserved: raw["startupReserved"].as_bool().unwrap(),
                    },
                    other => panic!("unknown modeled action {other}"),
                };
                let next = current.step(action);
                assert_eq!(
                    next.is_some(),
                    step["accepted"].as_bool().unwrap(),
                    "{case}"
                );
                if let Some(next) = next {
                    current = next;
                }
                assert_eq!(current, state(&step["state"]), "{case}");
            }
        }
    }
}
