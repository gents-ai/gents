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
mod tests {
    use super::*;
    #[test]
    fn handoff_matches_executable_lean_owner() {
        let snapshot: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
        let cases = snapshot["update_handoff_cases"].as_array().unwrap();
        assert!(!cases.is_empty());
        for case in cases {
            let phase: Phase = serde_json::from_value(case["phase"].clone()).unwrap();
            let flag = case["flag"].as_bool().unwrap();
            assert_eq!(
                phase.can_restore_prior(flag, false),
                case["can_restore"].as_bool().unwrap(),
                "{case}"
            );
            assert_eq!(
                phase.can_restore_prior(flag, true),
                case["can_restore_same_build"].as_bool().unwrap(),
                "{case}"
            );
            let action = match case["action"].as_str().unwrap() {
                "register" => Action::Register(flag),
                "acknowledge" => Action::Acknowledge,
                "retire" => Action::Retire(flag),
                "release" => Action::Release(flag),
                "backup" => Action::Backup(flag),
                "install" => Action::Install(flag),
                "verify" => Action::Verify(flag),
                "fail" => Action::Fail,
                other => panic!("unknown modeled action {other}"),
            };
            let expected: Option<Phase> = serde_json::from_value(case["next"].clone()).unwrap();
            assert_eq!(phase.step(action), expected, "{case}");
        }
    }
}
