//! Why a hosted install session failed, as a fixed class.
//!
//! A runtime-side error chain can quote what it choked on: an install
//! variable's resolved value, a rendered document field, a GraphQL error that
//! echoes a document. That text must not cross the wire, reach the client's
//! terminal or a model, or be logged. So the runtime sends a class from this
//! fixed vocabulary, and the text that goes with it is either the class's own
//! value-free description or, where the runtime holds a typed refusal written
//! from identifiers and build facts alone, that refusal's own text (never the
//! chain around it). The runtime decides that where it holds the typed value,
//! so nothing here classifies an error by downcasting it.

use serde::{Deserialize, Serialize};

/// The step of a hosted install that failed. Each class has a fixed,
/// value-free description that names the next step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackSessionFailureClass {
    /// The caller may not update the runtime's Node.
    NotAuthorized,
    /// The start request was missing, malformed, for another Node, over the
    /// size bound, or its archives did not match the root's dependency list.
    InvalidRequest,
    /// The client and the runtime are different builds. Its text is shown: it
    /// holds build facts (version, commit, core-pack digests) and no plan value.
    BuildMismatch,
    /// A Tools document names a plugin the runtime does not hold, or pins one
    /// to another digest. Its text is shown: it names the pack, the Tools id,
    /// the plugin and digests, which are identifiers, not values.
    PluginUnresolved,
    /// Documents the pack would write already exist on the Node with other
    /// content. Its text is shown: it names the pack and each document by
    /// collection and id, and the overwrite or keep choice.
    DocumentsDiffer,
    /// Documents of the pack were edited since it was installed. Its text is
    /// shown, for the same reason.
    DocumentsEdited,
    /// The runtime could not prepare the plan from the received packs.
    PreparationFailed,
    /// The client did not finish its schema preparation, or its evidence did
    /// not verify.
    SchemaPreparation,
    /// A schema of the pack or a dependency could not be applied.
    SchemaApplyFailed,
    /// An effect of the plan was refused; nothing was installed.
    ApplyRejected,
    /// The apply stopped where its outcome cannot be confirmed.
    ApplyUnconfirmed,
    /// The install applied, but its receipt could not be assembled or sent.
    ReceiptFailed,
    /// The runtime could not compute the preview.
    PreviewFailed,
}

impl PackSessionFailureClass {
    /// Whether the runtime's own text for this class is shown. True only where
    /// that text is a typed error the runtime writes from identifiers and build
    /// facts, never from a field value or an install variable.
    pub fn shows_runtime_text(self) -> bool {
        matches!(
            self,
            Self::BuildMismatch
                | Self::PluginUnresolved
                | Self::DocumentsDiffer
                | Self::DocumentsEdited
        )
    }

    /// The fixed, value-free description, with the next step.
    pub fn description(self) -> &'static str {
        match self {
            Self::NotAuthorized => {
                "this identity may not update the runtime's Node; install as the Node's User"
            }
            Self::InvalidRequest => {
                "the runtime could not accept the install request (another Node, over the size \
                 bound, or archives that do not match the root's dependencies); check the \
                 --graphql target and the pack's dependencies"
            }
            Self::BuildMismatch => {
                "this client and the runtime are different builds; run `gents runtime service \
                 update`, or install from a client of the runtime's build"
            }
            Self::PluginUnresolved => {
                "a Tools document names a plugin the runtime does not hold, or pins one to another \
                 digest; install the pack that ships it on the runtime first"
            }
            Self::DocumentsDiffer => {
                "documents the pack would write already exist on the Node with other content; \
                 rename or remove them, or decide with --overwrite or --keep on the same command"
            }
            Self::DocumentsEdited => {
                "documents of the pack were edited since it was installed; decide with \
                 --overwrite or --keep on the same command"
            }
            Self::PreparationFailed => {
                "the runtime could not prepare the install plan from the received packs (an \
                 install variable, inference slot, bound agent or pin move that does not resolve \
                 on the runtime); check the values this install passes for them, then retry"
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
            Self::PreviewFailed => {
                "the runtime could not compute the preview (an install variable, inference slot, \
                 bound agent or pin move that does not resolve on the runtime); check the values \
                 this preview passes for them, then retry"
            }
        }
    }

    /// The class named on the wire, or `None` for one this build does not know.
    pub fn from_wire(code: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(code.to_owned())).ok()
    }

    /// Every class, for tests that check the whole vocabulary.
    pub const ALL: [Self; 13] = [
        Self::NotAuthorized,
        Self::InvalidRequest,
        Self::BuildMismatch,
        Self::PluginUnresolved,
        Self::DocumentsDiffer,
        Self::DocumentsEdited,
        Self::PreparationFailed,
        Self::SchemaPreparation,
        Self::SchemaApplyFailed,
        Self::ApplyRejected,
        Self::ApplyUnconfirmed,
        Self::ReceiptFailed,
        Self::PreviewFailed,
    ];
}

/// A failed hosted session: the class sent, the chain kept on the runtime,
/// and whether anything may have been installed.
pub(crate) struct SessionFailure {
    pub class: PackSessionFailureClass,
    /// What the runtime may show: a typed refusal's own text, set only where
    /// the runtime holds that refusal and its class shows runtime text.
    shown: Option<String>,
    pub outcome: &'static str,
}

impl SessionFailure {
    /// A failure before anything was written. The chain is dropped: it can
    /// quote install values, so it is neither sent nor logged.
    pub fn not_installed(class: PackSessionFailureClass) -> impl FnOnce(anyhow::Error) -> Self {
        move |_chain| Self {
            class,
            shown: None,
            outcome: "not_installed",
        }
    }

    /// A failure after the apply started, whose outcome is not confirmed.
    pub fn unconfirmed(class: PackSessionFailureClass) -> impl FnOnce(anyhow::Error) -> Self {
        move |_chain| Self {
            class,
            shown: None,
            outcome: "unconfirmed",
        }
    }

    /// A typed refusal the runtime holds, shown as written. Only a class that
    /// shows runtime text carries one; any other shows its description.
    pub fn shown(class: PackSessionFailureClass, text: String, outcome: &'static str) -> Self {
        Self {
            class,
            shown: class.shows_runtime_text().then_some(text),
            outcome,
        }
    }

    /// A refused pack change the runtime shows as written, matched by variant:
    /// each names identifiers only (the pack, documents by collection and id,
    /// plugin coordinates and digests). `None` for any other refusal.
    pub fn change_refusal(error: &gents_pack::pack::PackChangeError) -> Option<Self> {
        use gents_pack::pack::PackChangeError;
        let class = match error {
            PackChangeError::Differing { .. } => PackSessionFailureClass::DocumentsDiffer,
            PackChangeError::Edited { .. } => PackSessionFailureClass::DocumentsEdited,
            PackChangeError::PluginReference(_) => PackSessionFailureClass::PluginUnresolved,
            _ => return None,
        };
        Some(Self::shown(class, error.to_string(), "not_installed"))
    }

    /// A rejected hosted transaction: documents that differ or were edited are
    /// shown as written, any other rejection by its class's description.
    pub fn rejection(rejection: gents_pack::pack::HostedRejection) -> Self {
        use gents_pack::pack::HostedRejection;
        let class = match rejection {
            HostedRejection::Differing { .. } => PackSessionFailureClass::DocumentsDiffer,
            HostedRejection::Edited { .. } => PackSessionFailureClass::DocumentsEdited,
            HostedRejection::Other(error) => {
                return Self::not_installed(PackSessionFailureClass::ApplyRejected)(error)
            }
        };
        Self::shown(class, rejection.to_string(), "not_installed")
    }

    /// The `failed` message the client receives.
    pub fn message(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "failed",
            "error": self
                .shown
                .clone()
                .unwrap_or_else(|| self.class.description().to_owned()),
            "outcome": self.outcome,
            "class": self.class,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No class sends an error chain: a failure sends its class's description
    /// unless the runtime handed it a typed refusal to show, so a value quoted
    /// anywhere in the chain never leaves the runtime.
    #[test]
    fn no_class_sends_a_chain_that_quotes_a_value() {
        let secret = "sk-live-0123456789";
        for class in PackSessionFailureClass::ALL {
            let failure = SessionFailure::not_installed(class)(anyhow::anyhow!(
                "variable TOKEN resolved to {secret}"
            ));
            let message = failure.message().to_string();
            assert!(!message.contains(secret), "{class:?}: {message}");
            assert!(message.contains(class.description()), "{message}");
            assert_eq!(
                PackSessionFailureClass::from_wire(
                    serde_json::to_value(class).unwrap().as_str().unwrap()
                ),
                Some(class)
            );
        }
        assert_eq!(
            PackSessionFailureClass::from_wire("from_a_later_build"),
            None
        );
    }

    /// A typed refusal is shown as written only by a class that shows runtime
    /// text; handed to any other, its description goes instead.
    #[test]
    fn only_a_class_that_shows_text_sends_a_typed_refusal() {
        let refusal = gents_pack::pack::PluginReferenceUnresolved::NotInstalled {
            pack: "safety".into(),
            tools: "safety-tools".into(),
            plugin: "private/missing".into(),
            root: "gents/safety".into(),
        }
        .to_string();
        for class in PackSessionFailureClass::ALL {
            let message = SessionFailure::shown(class, refusal.clone(), "not_installed")
                .message()
                .to_string();
            assert_eq!(
                message.contains("private/missing"),
                class.shows_runtime_text(),
                "{class:?}: {message}"
            );
        }
    }

    /// Documents that differ or were edited reach the client as written: the
    /// pack, each document by collection and id, and the overwrite or keep
    /// choice, with nothing from the chain of any other rejection.
    #[test]
    fn documents_that_differ_or_were_edited_are_shown_by_name() {
        use gents_pack::pack::{HostedRejection, PackChangeError};
        let differing =
            SessionFailure::rejection(HostedRejection::of(PackChangeError::Differing {
                coordinate: "fixture/rooted".into(),
                differing: vec!["Tools/rooted-tools".into()],
            }));
        let edited = SessionFailure::rejection(HostedRejection::of(PackChangeError::Edited {
            coordinate: "fixture/rooted".into(),
            edited: vec!["Agent/did:key:zAgent".into()],
        }));
        for (failure, class, name) in [
            (
                &differing,
                PackSessionFailureClass::DocumentsDiffer,
                "Tools/rooted-tools",
            ),
            (
                &edited,
                PackSessionFailureClass::DocumentsEdited,
                "Agent/did:key:zAgent",
            ),
        ] {
            assert_eq!(failure.class, class);
            let error = failure.message()["error"].as_str().unwrap().to_owned();
            assert!(error.contains(name), "{error}");
            assert!(error.contains("fixture/rooted"), "{error}");
            assert!(error.contains("--overwrite or --keep"), "{error}");
        }
        // Any other rejection, even one quoting a value, sends the description.
        let other = SessionFailure::rejection(HostedRejection::of(PackChangeError::Other(
            anyhow::anyhow!("field resolved to sk-live-0123456789"),
        )));
        assert_eq!(other.class, PackSessionFailureClass::ApplyRejected);
        assert!(!other.message().to_string().contains("sk-live"));
    }

    /// A remote operator cannot use the runtime's home, so no description
    /// points there; and no description has a lost line continuation.
    #[test]
    fn descriptions_name_only_steps_a_remote_operator_can_take() {
        for class in PackSessionFailureClass::ALL {
            let text = class.description();
            assert!(!text.contains("--home"), "{class:?}: {text}");
            assert!(!text.contains("  "), "{class:?}: {text}");
        }
    }
}
