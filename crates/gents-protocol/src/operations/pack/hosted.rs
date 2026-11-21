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
    UnsetVariables,
    PluginUnresolved,
    DocumentsDiffer,
    DocumentsEdited,
}

impl HostedInstallFailure {
    pub const ALL: [Self; 15] = [
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
        Self::UnsetVariables,
        Self::PluginUnresolved,
        Self::DocumentsDiffer,
        Self::DocumentsEdited,
    ];

    /// Lean `Pack.HostedRefusal.showsRuntimeText`: whether the runtime's text
    /// for this class crosses the wire, when every identifier the received
    /// archives author is `literal`.
    pub fn shows_runtime_text(self, literal: bool) -> bool {
        match self {
            Self::UnsetVariables => true,
            Self::PluginUnresolved | Self::DocumentsDiffer | Self::DocumentsEdited => literal,
            Self::RuntimeUnavailable
            | Self::NotAuthorized
            | Self::RefusedForAPerson
            | Self::InvalidRequest
            | Self::PreparationFailed
            | Self::PreviewFailed
            | Self::SchemaPreparation
            | Self::SchemaApplyFailed
            | Self::ApplyRejected
            | Self::ApplyUnconfirmed
            | Self::ReceiptFailed => false,
        }
    }

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
            Self::UnsetVariables => {
                "the pack reads install variables that have no value; supply each as NAME=VALUE, \
                 or give it a value in the installing environment"
            }
            Self::PluginUnresolved => {
                "a Tools document names a plugin the runtime does not hold, or pins one to \
                 another digest; install the pack that ships it on the runtime first"
            }
            Self::DocumentsDiffer => {
                "documents the pack would write already exist on the Node with other content; \
                 rename or remove them, or retry the same install with --overwrite or --keep"
            }
            Self::DocumentsEdited => {
                "documents of the pack were edited since it was installed; retry the same \
                 install with --overwrite or --keep"
            }
        }
    }
}

/// Lean `Pack.HostedRefusal.namesEditedDocument`: whether an edited document
/// is named in a refusal rather than counted.
pub fn names_edited_document(literal: bool, shipped: bool) -> bool {
    literal && shipped
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
    fn disclosure_matches_the_lean_model() {
        let cases = gents_lean_vocab::families::pack_hosted_refusal_shows_runtime_text::cases();
        for class in HostedInstallFailure::ALL {
            assert!(
                cases
                    .iter()
                    .any(|case| serde_json::to_value(&case.input.failure).unwrap()
                        == serde_json::to_value(class).unwrap()),
                "{class} is not a Lean class"
            );
        }
        for case in cases {
            let class: HostedInstallFailure = case.input.failure;
            case.check(class.shows_runtime_text(case.input.literal));
        }
    }

    #[test]
    fn edited_naming_matches_the_lean_model() {
        gents_lean_vocab::families::pack_hosted_refusal_names_edited_document::replay(|input| {
            names_edited_document(input.literal, input.shipped)
        });
    }

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
