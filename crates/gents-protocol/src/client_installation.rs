#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(test, derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(test, serde(rename_all = "snake_case"))]
pub enum Phase {
    Accepting,
    Draining,
    Released,
    /// The installation's store lock is free as well.
    Closed,
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
    Close,
    Acknowledge,
    Replace { startup_reserved: bool },
}

pub fn can_reuse(
    protocol_matches: bool,
    frame_limit_matches: bool,
    required_commands: &[String],
    available_commands: &[String],
) -> bool {
    protocol_matches
        && frame_limit_matches
        && required_commands
            .iter()
            .all(|required| available_commands.contains(required))
}

impl State {
    pub fn step(self, action: Action) -> Option<Self> {
        let mut next = self;
        match action {
            Action::Attach if self.phase == Phase::Accepting => {
                next.attachments = self.attachments.checked_add(1)?;
            }
            Action::Detach
                if matches!(self.phase, Phase::Accepting | Phase::Draining)
                    && self.attachments > 0 =>
            {
                next.attachments -= 1;
            }
            Action::Retire if self.phase == Phase::Accepting => next.phase = Phase::Draining,
            Action::Release if self.phase == Phase::Draining && self.attachments == 0 => {
                next.phase = Phase::Released;
            }
            Action::Close if self.phase == Phase::Released => next.phase = Phase::Closed,
            Action::Acknowledge if self.phase == Phase::Closed => {}
            Action::Replace {
                startup_reserved: true,
            } if self.phase == Phase::Closed && self.attachments == 0 => {
                next.phase = Phase::Accepting;
            }
            _ => return None,
        }
        Some(next)
    }
}

#[cfg(test)]
mod compatibility_tests {
    use super::can_reuse;

    #[test]
    fn generated_installation_compatibility_matches_lean() {
        gents_lean_vocab::families::client_installation_compatibility::replay(|i| {
            can_reuse(
                i.protocol_matches,
                i.frame_limit_matches,
                &i.required_commands,
                &i.available_commands,
            )
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_lean_vocab::families::client_installation_traces::{StateWire, TraceStep};

    fn action(kind: &str, startup_reserved: bool) -> Action {
        match kind {
            "attach" => Action::Attach,
            "detach" => Action::Detach,
            "retire" => Action::Retire,
            "release" => Action::Release,
            "close" => Action::Close,
            "acknowledge" => Action::Acknowledge,
            "replace" => Action::Replace { startup_reserved },
            other => panic!("unknown modeled action {other}"),
        }
    }

    #[test]
    fn generated_installation_traces_preserve_draining_and_replacement_guards() {
        gents_lean_vocab::families::client_installation_traces::replay::<String, Phase>(|i| {
            let mut current = State {
                phase: i.phase,
                attachments: i.attachments as usize,
            };
            i.trace
                .iter()
                .map(|step| {
                    let next = current.step(action(&step.kind, step.startup_reserved));
                    let accepted = next.is_some();
                    if let Some(next) = next {
                        current = next;
                    }
                    TraceStep {
                        accepted,
                        state: StateWire {
                            phase: current.phase,
                            attachments: current.attachments as u64,
                        },
                    }
                })
                .collect()
        });
    }
}
