#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(test, derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(test, serde(rename_all = "snake_case"))]
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
mod conformance {
    use super::*;
    use gents_lean_vocab::families::runtime_service_replacement_traces as family;

    #[test]
    fn service_replacement_traces_match_lean() {
        family::replay::<String, Phase>(|scenario| {
            let mut current = State {
                phase: scenario.initial.phase,
                executions: scenario.initial.executions.try_into().unwrap(),
                native_outstanding: scenario.initial.native_outstanding.try_into().unwrap(),
            };
            let results = scenario
                .steps
                .iter()
                .map(|step| {
                    let action = match step.kind.as_str() {
                        "admit" => Action::Admit,
                        "finish" => Action::Finish,
                        "native_started" => Action::NativeStarted,
                        "native_returned" => Action::NativeReturned,
                        "retire" => Action::Retire(RetirementAuthority {
                            operator_authenticated: step.operator_authenticated,
                            observed_build_matches: step.observed_build_matches,
                        }),
                        "release" => Action::Release {
                            store_released: step.store_released,
                        },
                        "replace" => Action::Replace {
                            startup_reserved: step.startup_reserved,
                        },
                        other => panic!("unknown modeled action {other}"),
                    };
                    let next = current.step(action);
                    let accepted = next.is_some();
                    if let Some(next) = next {
                        current = next;
                    }
                    family::StepResult {
                        accepted,
                        state: family::State {
                            phase: current.phase,
                            executions: current.executions.try_into().unwrap(),
                            native_outstanding: current.native_outstanding.try_into().unwrap(),
                        },
                    }
                })
                .collect::<Vec<_>>();
            family::Outcome { results }
        });
    }
}
