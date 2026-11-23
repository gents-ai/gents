use std::collections::BTreeSet;

use gents_pack::pack::{
    EditedDocumentRefusal, HostedRejection, PackChangeError, PackChangeSurface,
};
use gents_protocol::operations::pack::hosted::{
    names_edited_document, HostedInstallFailure, HostedInstallOutcome,
};

/// A failed hosted pack session: what crosses the wire and what the runtime
/// logs. The error chain is dropped where the failure is made, because it can
/// quote install values; only a typed refusal's own text may be kept, as
/// [`HostedInstallFailure::shows_runtime_text`] decides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionFailure {
    pub class: HostedInstallFailure,
    pub outcome: HostedInstallOutcome,
    detail: Option<String>,
    documents: Option<EditedDocumentRefusal>,
}

impl SessionFailure {
    pub fn not_installed<E>(class: HostedInstallFailure) -> impl FnOnce(E) -> Self
    where
        E: Into<anyhow::Error>,
    {
        move |error| Self::of(class, HostedInstallOutcome::NotInstalled, error.into())
    }

    pub fn unconfirmed<E>(class: HostedInstallFailure) -> impl FnOnce(E) -> Self
    where
        E: Into<anyhow::Error>,
    {
        move |error| Self::of(class, HostedInstallOutcome::Unconfirmed, error.into())
    }

    fn of(
        class: HostedInstallFailure,
        outcome: HostedInstallOutcome,
        error: anyhow::Error,
    ) -> Self {
        let class =
            if gents_store::config_client::user_write::UserWriteRefusal::in_chain(error.as_ref()) {
                HostedInstallFailure::RefusedForAPerson
            } else {
                class
            };
        Self {
            class,
            outcome,
            detail: None,
            documents: None,
        }
    }

    /// A refusal the runtime holds as a typed value, with its text where the
    /// class may show it.
    fn shown(class: HostedInstallFailure, text: String, identifiers: &RefusalIdentifiers) -> Self {
        Self {
            class,
            outcome: HostedInstallOutcome::NotInstalled,
            detail: class
                .shows_runtime_text(identifiers.literal)
                .then_some(text),
            documents: None,
        }
    }

    /// A failed preparation or preview: a typed refusal by its own class,
    /// anything else by `step`.
    pub fn change(
        step: HostedInstallFailure,
        error: PackChangeError,
        identifiers: &RefusalIdentifiers,
    ) -> Self {
        let hosted = error.message_for_surface(PackChangeSurface::Hosted);
        match error {
            PackChangeError::Edited { coordinate, edited } => {
                identifiers.edited(coordinate, &edited)
            }
            PackChangeError::Differing { .. } => {
                Self::shown(HostedInstallFailure::DocumentsDiffer, hosted, identifiers)
            }
            PackChangeError::PluginReference(_) => {
                Self::shown(HostedInstallFailure::PluginUnresolved, hosted, identifiers)
            }
            other => {
                let error = other.into_anyhow();
                match gents_pack::pack::interpolate::UnsetVariables::in_chain(error.as_ref()) {
                    Some(unset) => Self::shown(
                        HostedInstallFailure::UnsetVariables,
                        unset.to_string(),
                        identifiers,
                    ),
                    None => Self::not_installed(step)(error),
                }
            }
        }
    }

    /// A rejected hosted document transaction.
    pub fn rejection(rejection: HostedRejection, identifiers: &RefusalIdentifiers) -> Self {
        match rejection {
            HostedRejection::Edited { coordinate, edited } => {
                identifiers.edited(coordinate, &edited)
            }
            differing @ HostedRejection::Differing { .. } => Self::shown(
                HostedInstallFailure::DocumentsDiffer,
                differing.to_string(),
                identifiers,
            ),
            HostedRejection::Other(error) => {
                Self::not_installed(HostedInstallFailure::ApplyRejected)(error)
            }
        }
    }

    pub fn log(&self) {
        tracing::warn!(class = %self.class, outcome = %self.outcome, "hosted pack session failed");
    }

    pub fn message(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "failed",
            "class": self.class,
            "outcome": self.outcome,
            "detail": self.detail,
            "documents": self.documents,
        })
    }

    pub fn body(&self) -> serde_json::Value {
        serde_json::json!({
            "class": self.class,
            "outcome": self.outcome,
            "detail": self.detail,
        })
    }
}

/// Whether the received archives author every identifier a refusal can name
/// literally, and the documents their prepared plan ships, as
/// `Collection/id`.
#[derive(Clone, Debug, Default)]
pub(crate) struct RefusalIdentifiers {
    literal: bool,
    shipped: BTreeSet<String>,
}

impl RefusalIdentifiers {
    pub fn of(packs: &[&gents_pack::pack::resolved_pack::ResolvedPack]) -> Self {
        Self {
            literal: packs.iter().all(|pack| {
                pack.manifest().config.as_deref().is_none_or(|path| {
                    pack.archive()
                        .asset(path)
                        .ok()
                        .and_then(|bytes| serde_json::from_slice(bytes).ok())
                        .is_some_and(|config| literal_config(&config))
                })
            }),
            shipped: BTreeSet::new(),
        }
    }

    pub fn ship(
        &mut self,
        prepared: &gents_pack::pack::PreparedDocumentPackInstall,
    ) -> serde_json::Result<()> {
        use gents_store::collection::Collection;
        for config in std::iter::once(&prepared.config).chain(
            prepared
                .dependencies
                .iter()
                .map(|dependency| &dependency.config),
        ) {
            let config = serde_json::to_value(config)?;
            for collection in Collection::ALL {
                let rows = collection
                    .dir_name()
                    .and_then(|dir| config.get(dir))
                    .and_then(serde_json::Value::as_array);
                for row in rows.into_iter().flatten() {
                    if let Some(id) = row[collection.unique_field()].as_str() {
                        self.shipped
                            .insert(format!("{}/{id}", collection.graphql_type()));
                    }
                }
            }
        }
        Ok(())
    }

    fn edited(&self, coordinate: String, edited: &[String]) -> SessionFailure {
        let named: Vec<String> = edited
            .iter()
            .filter(|name| names_edited_document(self.literal, self.shipped.contains(*name)))
            .cloned()
            .collect();
        let documents = EditedDocumentRefusal {
            coordinate,
            unnamed: edited.len() - named.len(),
            named,
        };
        let mut failure = SessionFailure::shown(
            HostedInstallFailure::DocumentsEdited,
            documents.message_for_surface(PackChangeSurface::Hosted),
            self,
        );
        failure.documents = Some(documents);
        failure
    }
}

/// Whether every identifier `config` authors is literal: each document's
/// unique id, and each Tools plugin reference's coordinate and digest.
fn literal_config(config: &serde_json::Value) -> bool {
    use gents_store::collection::Collection;
    let literal =
        |value: &serde_json::Value| value.as_str().is_none_or(|text| !text.contains("${"));
    Collection::ALL.into_iter().all(|collection| {
        collection
            .dir_name()
            .and_then(|dir| config.get(dir))
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .all(|row| {
                literal(&row[collection.unique_field()])
                    && (collection != Collection::Tools
                        || row
                            .pointer("/integrations/plugins")
                            .and_then(serde_json::Value::as_array)
                            .into_iter()
                            .flatten()
                            .all(|reference| {
                                literal(&reference["plugin"]) && literal(&reference["digest"])
                            }))
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_pack::pack::PluginReferenceUnresolved;
    use serde_json::json;

    fn identifiers(literal: bool, shipped: &[&str]) -> RefusalIdentifiers {
        RefusalIdentifiers {
            literal,
            shipped: shipped.iter().map(|id| (*id).to_owned()).collect(),
        }
    }

    #[test]
    fn a_failure_sends_and_logs_its_class_never_its_chain() {
        let secret = "sk-live-0123456789";
        for class in HostedInstallFailure::ALL {
            let failure =
                SessionFailure::not_installed(class)(anyhow::anyhow!("TOKEN resolved to {secret}"));
            assert_eq!(failure.class, class);
            assert_eq!(failure.outcome, HostedInstallOutcome::NotInstalled);
            let message = failure.message();
            assert!(!message.to_string().contains(secret), "{message}");
            assert_eq!(message["detail"], serde_json::Value::Null);
        }
    }

    #[test]
    fn a_refused_write_for_a_person_is_its_own_class_whatever_step_failed() {
        let refusal = anyhow::Error::new(gents_store::config_client::user_write::RefreshRefused)
            .context("applying the plan");
        let failure = SessionFailure::unconfirmed(HostedInstallFailure::ApplyUnconfirmed)(refusal);
        assert_eq!(failure.class, HostedInstallFailure::RefusedForAPerson);
        assert_eq!(failure.outcome, HostedInstallOutcome::Unconfirmed);
    }

    #[test]
    fn typed_refusals_show_their_names_only_from_literal_archives() {
        let plugin = || {
            PackChangeError::PluginReference(PluginReferenceUnresolved::NotInstalled {
                pack: "safety".into(),
                tools: "safety-tools".into(),
                plugin: "private/missing".into(),
                root: "gents/safety".into(),
            })
        };
        let differing = || PackChangeError::Differing {
            coordinate: "gents/safety".into(),
            differing: vec!["Tools/safety-tools".into()],
        };
        let unset = || {
            PackChangeError::Other(
                anyhow::Error::new(gents_pack::pack::interpolate::UnsetVariables {
                    names: vec!["API_TOKEN".into()],
                })
                .context("loading the pack"),
            )
        };
        for literal in [true, false] {
            let ids = identifiers(literal, &[]);
            for (error, class, name) in [
                (
                    plugin(),
                    HostedInstallFailure::PluginUnresolved,
                    "safety-tools",
                ),
                (
                    differing(),
                    HostedInstallFailure::DocumentsDiffer,
                    "Tools/safety-tools",
                ),
                (unset(), HostedInstallFailure::UnsetVariables, "API_TOKEN"),
            ] {
                let failure =
                    SessionFailure::change(HostedInstallFailure::PreparationFailed, error, &ids);
                assert_eq!(failure.class, class);
                let message = failure.message();
                let shown = message["detail"].as_str().unwrap_or_default();
                assert_eq!(
                    shown.contains(name),
                    class.shows_runtime_text(literal),
                    "{class} literal={literal}: {message}"
                );
                assert!(
                    !shown.contains("pack update") && !shown.contains("--home"),
                    "{shown}"
                );
            }
        }
        let other = SessionFailure::change(
            HostedInstallFailure::PreviewFailed,
            PackChangeError::Other(anyhow::anyhow!("field resolved to sk-live-0123")),
            &identifiers(true, &[]),
        );
        assert_eq!(other.class, HostedInstallFailure::PreviewFailed);
        assert!(!other.message().to_string().contains("sk-live"));
    }

    #[test]
    fn an_edited_refusal_names_shipped_literal_documents_and_counts_the_rest() {
        let edited = || PackChangeError::Edited {
            coordinate: "fixture/root".into(),
            edited: vec![
                "Tools/sk-live-prior".into(),
                "Tools/literal-tools".into(),
                "Tools/stale-tools".into(),
            ],
        };
        let ids = identifiers(true, &["Tools/literal-tools"]);
        for failure in [
            SessionFailure::change(HostedInstallFailure::PreparationFailed, edited(), &ids),
            SessionFailure::rejection(HostedRejection::of(edited()), &ids),
        ] {
            assert_eq!(failure.class, HostedInstallFailure::DocumentsEdited);
            let message = failure.message();
            assert_eq!(
                message["documents"],
                json!({"coordinate": "fixture/root", "named": ["Tools/literal-tools"], "unnamed": 2})
            );
            let text = message["detail"].as_str().unwrap();
            assert!(
                text.contains(": Tools/literal-tools and 2 others."),
                "{text}"
            );
            assert!(
                !message.to_string().contains("sk-live") && !text.contains("stale"),
                "{text}"
            );
        }
        let rendered = SessionFailure::change(
            HostedInstallFailure::PreparationFailed,
            edited(),
            &identifiers(false, &["Tools/literal-tools"]),
        );
        assert_eq!(rendered.documents.unwrap().named, Vec::<String>::new());
    }

    #[test]
    fn an_identifier_authored_with_a_variable_is_not_literal() {
        for pointer in [
            "/tools/0/tools_id",
            "/tools/0/integrations/plugins/0/plugin",
            "/tools/0/integrations/plugins/0/digest",
        ] {
            let mut config = json!({
                "node": {"node_did": "${GENTS_PACK_NODE_DID}"},
                "tools": [{"tools_id": "literal-tools", "display_name": "${SECRET}",
                    "integrations": {"plugins": [{"plugin": "fixture/p", "digest": "sha256:aa"}]}}]
            });
            assert!(literal_config(&config));
            *config.pointer_mut(pointer).unwrap() = json!("${IDENTIFIER}");
            assert!(!literal_config(&config), "{pointer}");
        }
    }
}
