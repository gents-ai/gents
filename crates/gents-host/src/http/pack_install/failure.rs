//! Why a hosted install session failed, as a fixed class.
//!
//! A runtime-side error chain can quote what it choked on: an install
//! variable's resolved value, a rendered document field, a GraphQL error that
//! echoes a document. That text must not cross the wire, reach the client's
//! terminal or a model, or be logged. So the runtime sends a class from this
//! fixed vocabulary, and the text that goes with it is either the class's own
//! value-free description or, for the one class whose text the runtime writes
//! itself from build facts alone, that text.

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
    /// The client and the runtime are different builds. The only class whose
    /// runtime-authored text is shown: it holds build facts (version, commit,
    /// core-pack digests) and no plan value.
    BuildMismatch,
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
    /// the runtime writes that text itself from values that are not the plan's.
    pub fn shows_runtime_text(self) -> bool {
        matches!(self, Self::BuildMismatch)
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
            Self::PreparationFailed => {
                "the runtime could not prepare the install plan from the received packs (an \
                 install variable, inference slot, bound agent or pin move that does not resolve \
                 on the runtime); preview the install on the runtime's home with \
                 `gents config pack install --preview --home <runtime home>` to see which"
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
                "an effect of the plan was refused and nothing was installed; retry the install, \
                 or preview it to see the plan"
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
                "the runtime could not compute the preview; preview the install on the runtime's \
                 home with `gents config pack install --preview --home <runtime home>`"
            }
        }
    }

    /// The text the runtime sends for this class: its own text only where the
    /// class allows it, otherwise the description. The error chain itself never
    /// crosses the wire.
    pub fn wire_text(self, error: &anyhow::Error) -> String {
        if self.shows_runtime_text() {
            format!("{error:#}")
        } else {
            self.description().to_owned()
        }
    }

    /// The class named on the wire, or `None` for one this build does not know.
    pub fn from_wire(code: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(code.to_owned())).ok()
    }

    /// Every class, for tests that check the whole vocabulary.
    pub const ALL: [Self; 10] = [
        Self::NotAuthorized,
        Self::InvalidRequest,
        Self::BuildMismatch,
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
    pub error: anyhow::Error,
    pub outcome: &'static str,
}

impl SessionFailure {
    /// A failure before anything was written.
    pub fn not_installed(class: PackSessionFailureClass) -> impl FnOnce(anyhow::Error) -> Self {
        move |error| Self {
            class,
            error,
            outcome: "not_installed",
        }
    }

    /// A failure after the apply started, whose outcome is not confirmed.
    pub fn unconfirmed(class: PackSessionFailureClass) -> impl FnOnce(anyhow::Error) -> Self {
        move |error| Self {
            class,
            error,
            outcome: "unconfirmed",
        }
    }

    /// The `failed` message the client receives.
    pub fn message(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "failed",
            "error": self.class.wire_text(&self.error),
            "outcome": self.outcome,
            "class": self.class,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No class but the build mismatch sends the runtime's text, so a value
    /// quoted by an error chain never leaves the runtime.
    #[test]
    fn only_the_build_mismatch_sends_runtime_text() {
        let secret = "sk-live-0123456789";
        let error = anyhow::anyhow!("variable TOKEN resolved to {secret}");
        for class in PackSessionFailureClass::ALL {
            let failure = SessionFailure::not_installed(class)(anyhow::anyhow!("{error:#}"));
            let message = failure.message().to_string();
            if class.shows_runtime_text() {
                assert_eq!(class, PackSessionFailureClass::BuildMismatch);
            } else {
                assert!(!message.contains(secret), "{class:?}: {message}");
                assert!(message.contains(class.description()), "{message}");
            }
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
}
