use serde::{Deserialize, Serialize};

/// The step of a hosted pack install or wire that failed, as it crosses the
/// wire in place of the runtime's error chain, which can quote install
/// values and is neither sent nor logged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostedInstallFailure {
    RuntimeUnavailable,
    NotAuthorized,
    RefusedForAPerson,
    InvalidRequest,
    PreparationFailed,
    PreviewFailed,
    SchemaPreparation,
    SchemaApplyFailed,
    ApplyRejected,
    ApplyUnconfirmed,
    ReceiptFailed,
}

impl HostedInstallFailure {
    pub const ALL: [Self; 11] = [
        Self::RuntimeUnavailable,
        Self::NotAuthorized,
        Self::RefusedForAPerson,
        Self::InvalidRequest,
        Self::PreparationFailed,
        Self::PreviewFailed,
        Self::SchemaPreparation,
        Self::SchemaApplyFailed,
        Self::ApplyRejected,
        Self::ApplyUnconfirmed,
        Self::ReceiptFailed,
    ];

    /// What the class means to the installer, with the next step.
    pub fn description(self) -> &'static str {
        match self {
            Self::RuntimeUnavailable => {
                "the runtime is not ready to install packs (starting, or serving no plugin store); \
                 retry once `gents runtime status` reports it ready"
            }
            Self::NotAuthorized => {
                "this identity may not update the runtime's Node; install as the Node's User"
            }
            Self::RefusedForAPerson => crate::failure_text::REFUSED_FOR_A_PERSON,
            Self::InvalidRequest => {
                "the runtime could not accept the install request (another Node, over the size \
                 bound, or archives that do not match the root's dependencies); check the \
                 --graphql target and the pack's dependencies"
            }
            Self::PreparationFailed => {
                "the runtime could not prepare the install plan from the received packs (an \
                 install variable, inference slot, bound agent or plugin that does not resolve \
                 on the runtime); check the values this install passes for them, then retry"
            }
            Self::PreviewFailed => {
                "the runtime could not compute the preview (an install variable, inference \
                 slot, bound agent or plugin that does not resolve on the runtime); check the \
                 values this preview passes for them, then retry"
            }
            Self::SchemaPreparation => {
                "the client's schema preparation did not finish or did not verify; retry the \
                 install"
            }
            Self::SchemaApplyFailed => {
                "a schema of the pack or one of its dependencies could not be applied; check the \
                 pack's schemas against the collections already on the Node"
            }
            Self::ApplyRejected => {
                "an effect of the plan was refused and nothing was installed; add --preview to \
                 the same command to see the plan"
            }
            Self::ApplyUnconfirmed => {
                "the install stopped where its outcome cannot be confirmed; inspect the Node \
                 (`gents config pack list`) before retrying"
            }
            Self::ReceiptFailed => {
                "the install applied but its receipt could not be assembled or sent; inspect the \
                 Node (`gents config pack list`) before retrying"
            }
        }
    }
}

impl std::fmt::Display for HostedInstallFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.serialize(f)
    }
}

/// Whether anything of a failed hosted install may have been written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostedInstallOutcome {
    NotInstalled,
    Unconfirmed,
}

impl std::fmt::Display for HostedInstallOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.serialize(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_class_names_a_step_a_remote_installer_can_take() {
        for class in HostedInstallFailure::ALL {
            let text = class.description();
            assert!(!text.contains("--home"), "{class}: {text}");
            assert!(!text.contains("  "), "{class}: {text}");
            assert_eq!(
                serde_json::from_value::<HostedInstallFailure>(serde_json::to_value(class).unwrap())
                    .unwrap(),
                class
            );
        }
        assert_eq!(HostedInstallFailure::ApplyRejected.to_string(), "apply_rejected");
        assert_eq!(HostedInstallOutcome::NotInstalled.to_string(), "not_installed");
    }
}
