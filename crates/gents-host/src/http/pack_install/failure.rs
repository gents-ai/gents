//! Why a hosted install session failed, as a fixed class.
//!
//! A runtime-side error chain can quote what it choked on: an install
//! variable's resolved value, a rendered document field, a GraphQL error that
//! echoes a document. That text must not cross the wire, reach the client's
//! terminal or a model, or be logged. So the runtime sends a class from this
//! fixed vocabulary, and the text that goes with it is either the class's own
//! value-free description or, for a class whose error is a typed one the
//! runtime writes from identifiers and build facts alone, that error's own
//! text (never the chain around it).

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
    /// Documents of the pack already exist on the Node with other content, or
    /// were edited since it was installed. Its text is shown: it names the pack
    /// and each document by collection and id.
    DocumentsDiffer,
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
            Self::BuildMismatch | Self::PluginUnresolved | Self::DocumentsDiffer
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
                "documents of the pack already exist on the Node with other content or were \
                 edited; decide with --overwrite or --keep on `gents config pack install`"
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

    /// The text the runtime sends for this class: the typed error's own text
    /// where the class allows it, otherwise the description. The error chain
    /// around it never crosses the wire, and an error of another type than the
    /// class names falls back to the description.
    pub fn wire_text(self, error: &anyhow::Error) -> String {
        let typed = match self {
            Self::BuildMismatch => error
                .downcast_ref::<crate::http::pack_install_client::PackInstallHandshakeError>()
                .map(ToString::to_string),
            Self::PluginUnresolved => plugin_unresolved(error).map(ToString::to_string),
            Self::DocumentsDiffer => documents_differ(error).map(ToString::to_string),
            _ => None,
        };
        typed.unwrap_or_else(|| self.description().to_owned())
    }

    /// The class of `error`: the typed refusal it carries when it carries one
    /// whose text is shown, otherwise `general`, the class of the step.
    pub fn of(general: Self, error: &anyhow::Error) -> Self {
        if plugin_unresolved(error).is_some() {
            Self::PluginUnresolved
        } else if documents_differ(error).is_some() {
            Self::DocumentsDiffer
        } else {
            general
        }
    }

    /// The class named on the wire, or `None` for one this build does not know.
    pub fn from_wire(code: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(code.to_owned())).ok()
    }

    /// Every class, for tests that check the whole vocabulary.
    pub const ALL: [Self; 12] = [
        Self::NotAuthorized,
        Self::InvalidRequest,
        Self::BuildMismatch,
        Self::PluginUnresolved,
        Self::DocumentsDiffer,
        Self::PreparationFailed,
        Self::SchemaPreparation,
        Self::SchemaApplyFailed,
        Self::ApplyRejected,
        Self::ApplyUnconfirmed,
        Self::ReceiptFailed,
        Self::PreviewFailed,
    ];
}

/// The errors a hosted install wraps a typed refusal in: the stage's own
/// result and the pack change error's catch-all. Their `source()` skips the
/// inner error (they are transparent), so they are unwrapped by type.
fn unwrap_hosted(error: &anyhow::Error) -> Option<&anyhow::Error> {
    if let Some(
        gents_pack::pack::HostedPackStageError::Rejected(inner)
        | gents_pack::pack::HostedPackStageError::Unconfirmed(inner),
    ) = error.downcast_ref::<gents_pack::pack::HostedPackStageError>()
    {
        return Some(inner);
    }
    if let Some(gents_pack::pack::PackChangeError::Other(inner)) =
        error.downcast_ref::<gents_pack::pack::PackChangeError>()
    {
        return Some(inner);
    }
    None
}

fn plugin_unresolved(
    error: &anyhow::Error,
) -> Option<&gents_pack::pack::PluginReferenceUnresolved> {
    error
        .downcast_ref::<gents_pack::pack::PluginReferenceUnresolved>()
        .or_else(|| unwrap_hosted(error).and_then(plugin_unresolved))
}

fn documents_differ(error: &anyhow::Error) -> Option<&gents_pack::pack::PackChangeError> {
    match error.downcast_ref::<gents_pack::pack::PackChangeError>() {
        Some(
            change @ (gents_pack::pack::PackChangeError::Edited { .. }
            | gents_pack::pack::PackChangeError::Differing { .. }),
        ) => Some(change),
        _ => unwrap_hosted(error).and_then(documents_differ),
    }
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

    /// No class sends an error chain: a class whose text is shown sends only
    /// its typed error, so a value quoted anywhere else never leaves the
    /// runtime.
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

    /// A typed plugin error is shown as written, without the context around
    /// it, and preparation failures are classed by it.
    #[test]
    fn a_typed_plugin_error_is_shown_without_its_context() {
        let error = anyhow::Error::new(gents_pack::pack::PluginReferenceUnresolved::NotInstalled {
            pack: "safety".into(),
            tools: "safety-tools".into(),
            plugin: "private/missing".into(),
            root: "gents/safety".into(),
        })
        .context("preparing with TOKEN=sk-live-0123456789");
        let class = PackSessionFailureClass::of(PackSessionFailureClass::PreparationFailed, &error);
        assert_eq!(class, PackSessionFailureClass::PluginUnresolved);
        let text = class.wire_text(&error);
        assert!(text.contains("private/missing"), "{text}");
        assert!(!text.contains("sk-live"), "{text}");
        assert_eq!(
            PackSessionFailureClass::of(
                PackSessionFailureClass::PreparationFailed,
                &anyhow::anyhow!("other")
            ),
            PackSessionFailureClass::PreparationFailed
        );
    }

    /// Documents that differ are found inside the stage's rejection, through
    /// the transparent wrappers whose `source()` would skip them.
    #[test]
    fn differing_documents_are_found_inside_a_rejected_stage() {
        let differing = gents_pack::pack::PackChangeError::Differing {
            coordinate: "fixture/rooted".into(),
            differing: vec!["Agent/did:key:zAgent".into()],
        };
        let rejected = anyhow::Error::new(gents_pack::pack::HostedPackStageError::Rejected(
            anyhow::Error::new(gents_pack::pack::PackChangeError::Other(
                anyhow::Error::new(differing).context("with TOKEN=sk-live-0123456789"),
            ))
            .context("pack document transaction was rejected"),
        ));
        let class = PackSessionFailureClass::of(PackSessionFailureClass::ApplyRejected, &rejected);
        assert_eq!(class, PackSessionFailureClass::DocumentsDiffer);
        let text = class.wire_text(&rejected);
        assert!(text.contains("Agent/did:key:zAgent"), "{text}");
        assert!(text.contains("--overwrite or --keep"), "{text}");
        assert!(!text.contains("sk-live"), "{text}");
    }
}
