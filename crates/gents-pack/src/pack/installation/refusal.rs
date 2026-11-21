use gents_protocol::operations::pack::hosted::{HostedInstallFailure, HostedInstallOutcome};
use serde::{Deserialize, Serialize};

/// Where a refused pack change is read, which decides the step it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackChangeSurface {
    Local,
    Hosted,
    Desktop,
}

/// An edited-documents refusal as a hosted install sends it: the documents
/// it may name, and how many others were edited.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct EditedDocumentRefusal {
    pub coordinate: String,
    pub named: Vec<String>,
    pub unnamed: usize,
}

impl EditedDocumentRefusal {
    pub fn message_for_surface(&self, surface: PackChangeSurface) -> String {
        document_change_message(&self.coordinate, &self.named, self.unnamed, true, surface)
    }
}

/// A hosted install the runtime refused, as its client holds it: the class,
/// whether anything may have been written, and the runtime's text where the
/// class may show it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "the runtime refused the install ({outcome}, {class}): {}",
    self.message_for_surface(PackChangeSurface::Hosted)
)]
pub struct HostedRefusal {
    pub class: HostedInstallFailure,
    pub outcome: HostedInstallOutcome,
    pub detail: Option<String>,
    pub documents: Option<EditedDocumentRefusal>,
}

impl HostedRefusal {
    /// Keeps the runtime's text only for a class that may show runtime text,
    /// and its documents only for an edited refusal.
    pub fn new(
        class: HostedInstallFailure,
        outcome: HostedInstallOutcome,
        detail: Option<String>,
        documents: Option<EditedDocumentRefusal>,
    ) -> Self {
        Self {
            class,
            outcome,
            detail: detail.filter(|_| class.shows_runtime_text(true)),
            documents: documents.filter(|_| class == HostedInstallFailure::DocumentsEdited),
        }
    }

    pub fn message_for_surface(&self, surface: PackChangeSurface) -> String {
        match (&self.documents, &self.detail) {
            (Some(documents), _) if surface != PackChangeSurface::Hosted => {
                documents.message_for_surface(surface)
            }
            (_, Some(detail)) => detail.clone(),
            (Some(documents), None) => documents.message_for_surface(surface),
            (None, None) => self.class.description().to_owned(),
        }
    }
}

pub(crate) fn document_change_message(
    coordinate: &str,
    named: &[String],
    unnamed: usize,
    edited: bool,
    surface: PackChangeSurface,
) -> String {
    let listed = match (named.join(", "), unnamed) {
        (names, _) if names.is_empty() => String::new(),
        (names, 0) => format!(": {names}"),
        (names, 1) => format!(": {names} and 1 other"),
        (names, rest) => format!(": {names} and {rest} others"),
    };
    let count = named.len() + unnamed;
    if edited {
        let (removal, step) = match surface {
            PackChangeSurface::Local => (
                " (a removal deletes them)",
                "Decide with --overwrite or --keep on `gents config pack install`, `gents config \
                 pack update` or `gents config pack remove`, or options.edited on the agent pack \
                 command",
            ),
            PackChangeSurface::Hosted => ("", "Retry the same install with --overwrite or --keep"),
            PackChangeSurface::Desktop => ("", "Choose Overwrite or Keep in this install review"),
        };
        format!(
            "{count} of {coordinate}'s documents were edited since it was installed{listed}. \
             Overwrite replaces them with the pack's versions{removal}; keep leaves them as \
             edited. {step}"
        )
    } else {
        let step = match surface {
            PackChangeSurface::Local => {
                "--overwrite or --keep on `gents config pack install`, options.edited on the \
                 agent pack command"
            }
            PackChangeSurface::Hosted => "retry the same install with --overwrite or --keep",
            PackChangeSurface::Desktop => "choose Overwrite or Keep in this install review",
        };
        format!(
            "{count} of {coordinate}'s documents already exist on this node with different \
             content{listed}; the pack adopts an existing document only when it matches. Rename \
             or remove them, or decide: overwrite replaces them with the pack's versions, keep \
             leaves them as they are ({step})"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_edited_refusal_names_what_it_may_and_counts_the_rest() {
        let refusal = |named: &[&str], unnamed| EditedDocumentRefusal {
            coordinate: "fixture/root".into(),
            named: named.iter().map(|name| (*name).to_owned()).collect(),
            unnamed,
        };
        for (named, unnamed, listed) in [
            (&[][..], 2, "2 of fixture/root's documents were edited since it was installed. "),
            (&["Tools/a"][..], 0, "installed: Tools/a. "),
            (&["Tools/a"][..], 1, "installed: Tools/a and 1 other. "),
            (&["Tools/a", "Tools/b"][..], 3, "5 of fixture/root's documents were edited since it was installed: Tools/a, Tools/b and 3 others. "),
        ] {
            let text = refusal(named, unnamed).message_for_surface(PackChangeSurface::Hosted);
            assert!(text.contains(listed), "{text}");
            assert!(text.ends_with("Retry the same install with --overwrite or --keep"), "{text}");
        }
        let desktop = refusal(&["Tools/a"], 0).message_for_surface(PackChangeSurface::Desktop);
        for local_only in ["gents config", "--overwrite", "options.edited"] {
            assert!(!desktop.contains(local_only), "{desktop}");
        }
    }

    #[test]
    fn a_hosted_refusal_keeps_runtime_text_only_where_its_class_may_show_it() {
        let documents = EditedDocumentRefusal {
            coordinate: "fixture/root".into(),
            named: vec!["Tools/a".into()],
            unnamed: 1,
        };
        let refused = |class| {
            HostedRefusal::new(
                class,
                HostedInstallOutcome::NotInstalled,
                Some("runtime text sk-live".into()),
                Some(documents.clone()),
            )
        };
        let hidden = refused(HostedInstallFailure::ApplyRejected);
        assert_eq!(
            (hidden.detail.as_deref(), hidden.documents.as_ref()),
            (None, None)
        );
        assert_eq!(
            hidden.to_string(),
            format!(
                "the runtime refused the install (not_installed, apply_rejected): {}",
                HostedInstallFailure::ApplyRejected.description()
            )
        );
        let edited = refused(HostedInstallFailure::DocumentsEdited);
        assert_eq!(
            edited.message_for_surface(PackChangeSurface::Desktop),
            documents.message_for_surface(PackChangeSurface::Desktop)
        );
        assert!(edited.to_string().ends_with("runtime text sk-live"));
    }

    #[test]
    fn the_local_text_names_every_local_step() {
        let edited = document_change_message(
            "gents/web",
            &["Tools/web-researcher-tools".into()],
            0,
            true,
            PackChangeSurface::Local,
        );
        assert_eq!(
            edited,
            "1 of gents/web's documents were edited since it was installed: \
             Tools/web-researcher-tools. Overwrite replaces them with the pack's versions (a \
             removal deletes them); keep leaves them as edited. Decide with --overwrite or --keep \
             on `gents config pack install`, `gents config pack update` or `gents config pack \
             remove`, or options.edited on the agent pack command"
        );
        let differing = document_change_message(
            "gents/web",
            &["Tools/t".into()],
            0,
            false,
            PackChangeSurface::Hosted,
        );
        assert!(
            differing.contains("Tools/t") && !differing.contains("gents config"),
            "{differing}"
        );
    }
}
