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
        matches!(self, Self::BuildMismatch | Self::PluginUnresolved)
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
                "an effect of the plan was refused and nothing was installed (most often a document \
                 of the pack that already exists on the Node with other content, or was edited \
                 since it was installed: decide with --overwrite or --keep); preview the install \
                 to see the plan"
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

    /// The class named on the wire, or `None` for one this build does not know.
    pub fn from_wire(code: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(code.to_owned())).ok()
    }

    /// Every class, for tests that check the whole vocabulary.
    pub const ALL: [Self; 11] = [
        Self::NotAuthorized,
        Self::InvalidRequest,
        Self::BuildMismatch,
        Self::PluginUnresolved,
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
}
