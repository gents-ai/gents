#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Accepting,
    Draining,
    Released,
}

/// Attachment counts are snapshots of the listener's existing task set. This
/// contract does not own a second attachment registry or any history state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct State {
    pub phase: Phase,
    pub attachments: usize,
}

#[derive(Clone, Copy, Debug)]
pub enum Action {
    Attach,
    Detach,
    Retire,
    Release,
    Replace { startup_reserved: bool },
}

impl State {
    pub fn step(self, action: Action) -> Option<Self> {
        let mut next = self;
        match action {
            Action::Attach if self.phase == Phase::Accepting => {
                next.attachments = self.attachments.checked_add(1)?;
            }
            Action::Detach if self.phase != Phase::Released && self.attachments > 0 => {
                next.attachments -= 1;
            }
            Action::Retire if self.phase == Phase::Accepting => next.phase = Phase::Draining,
            Action::Release if self.phase == Phase::Draining && self.attachments == 0 => {
                next.phase = Phase::Released;
            }
            Action::Replace {
                startup_reserved: true,
            } if self.phase == Phase::Released && self.attachments == 0 => {
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
                "draining" => Phase::Draining,
                "released" => Phase::Released,
                other => panic!("unknown modeled phase {other}"),
            },
            attachments: value["attachments"].as_u64().unwrap().try_into().unwrap(),
        }
    }

    #[test]
    fn generated_installation_traces_preserve_draining_and_replacement_guards() {
        let snapshot: Value = gents_lean_contract::load_contract_snapshot().unwrap();
        let cases = snapshot["client_installation_cases"].as_array().unwrap();
        assert!(!cases.is_empty());
        for case in cases {
            let mut current = state(&case["initial"]);
            for step in case["trace"].as_array().unwrap() {
                let action = match step["action"]["kind"].as_str().unwrap() {
                    "attach" => Action::Attach,
                    "detach" => Action::Detach,
                    "retire" => Action::Retire,
                    "release" => Action::Release,
                    "replace" => Action::Replace {
                        startup_reserved: step["action"]["startupReserved"].as_bool().unwrap(),
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
