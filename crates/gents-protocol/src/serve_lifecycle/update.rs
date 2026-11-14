//! Durable native-service handoff. Observations belong to the existing native
//! service, authenticated retirement and exclusive store owners.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Staged,
    Registered,
    Accepted,
    Retired,
    Released,
    BackedUp,
    Installed,
    Healthy,
    Failed,
}

#[derive(Clone, Copy, Debug)]
pub enum Action {
    Register(bool),
    Acknowledge,
    Retire(bool),
    Release(bool),
    Backup(bool),
    Install(bool),
    Verify(bool),
    Fail,
}

impl Phase {
    pub fn can_restore_prior(self, candidate_may_have_opened: bool, same_build: bool) -> bool {
        (!candidate_may_have_opened || same_build)
            && matches!(
                self,
                Self::Retired | Self::Released | Self::BackedUp | Self::Installed
            )
    }

    pub fn step(self, action: Action) -> Option<Self> {
        match (self, action) {
            (Self::Staged, Action::Register(true)) => Some(Self::Registered),
            (Self::Registered, Action::Acknowledge) => Some(Self::Accepted),
            (Self::Accepted, Action::Retire(true)) => Some(Self::Retired),
            (Self::Retired, Action::Release(true)) => Some(Self::Released),
            (Self::Released, Action::Backup(true)) => Some(Self::BackedUp),
            (Self::BackedUp, Action::Install(true)) => Some(Self::Installed),
            (Self::Installed, Action::Verify(true)) => Some(Self::Healthy),
            (phase, Action::Fail) if !matches!(phase, Self::Healthy | Self::Failed) => {
                Some(Self::Failed)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod conformance {
    use super::*;

    #[test]
    fn update_handoff_transitions_match_lean() {
        gents_lean_vocab::families::update_handoff_transitions::replay::<String, Phase>(|i| {
            let action = match i.action.as_str() {
                "register" => Action::Register(i.flag),
                "acknowledge" => Action::Acknowledge,
                "retire" => Action::Retire(i.flag),
                "release" => Action::Release(i.flag),
                "backup" => Action::Backup(i.flag),
                "install" => Action::Install(i.flag),
                "verify" => Action::Verify(i.flag),
                "fail" => Action::Fail,
                other => panic!("unknown modeled action {other}"),
            };
            gents_lean_vocab::families::update_handoff_transitions::HandoffVerdict {
                can_restore: i.phase.can_restore_prior(i.flag, false),
                can_restore_same_build: i.phase.can_restore_prior(i.flag, true),
                next: i.phase.step(action),
            }
        });
    }
}
