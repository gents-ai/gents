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
    /// shown: it names an edited document only where the received archives
    /// ship it under a literal identifier, and counts the rest, which can be
    /// documents of the previous installation.
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
    /// facts, never from a field value or an install variable, and every
    /// identifier it can name comes from the received archives (so
    /// `RefusalIdentifiers` can check it).
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

// Identifiers can themselves be rendered from install variables. Keep the
// refusal class and remedy, but not its identifier text, for such archives.
// An edited document is named only where the received archives ship it, by
// the key its prepared plan renders; the rest can be documents of the previous
// installation and are counted.
#[derive(Clone)]
pub(crate) struct RefusalIdentifiers {
    literal: bool,
    shipped: std::collections::BTreeSet<String>,
}

impl RefusalIdentifiers {
    pub fn from_packs(packs: &[&gents_pack::pack::resolved_pack::ResolvedPack]) -> Self {
        Self {
            shipped: std::collections::BTreeSet::new(),
            literal: packs.iter().all(|pack| {
                pack.manifest().config.as_deref().is_none_or(|path| {
                    pack.archive()
                        .asset(path)
                        .ok()
                        .and_then(|bytes| serde_json::from_slice(bytes).ok())
                        .is_some_and(|config| Self::in_config(&config))
                })
            }),
        }
    }

    fn in_config(config: &serde_json::Value) -> bool {
        use gents_store::collection::Collection;
        let is_literal =
            |value: &serde_json::Value| value.as_str().is_none_or(|text| !text.contains("${"));
        Collection::ALL.into_iter().all(|collection| {
            collection
                .dir_name()
                .and_then(|dir| config.get(dir))
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .all(|row| {
                    is_literal(&row[collection.unique_field()])
                        && (collection != Collection::Tools
                            || row
                                .pointer("/integrations/plugins")
                                .and_then(serde_json::Value::as_array)
                                .into_iter()
                                .flatten()
                                .all(|reference| {
                                    ["plugin", "digest"]
                                        .into_iter()
                                        .all(|key| is_literal(&reference[key]))
                                }))
                })
        })
    }

    /// Records the documents the prepared plan ships, root and rendered
    /// dependencies, as `Collection/id` with the ids the loader rendered.
    pub fn ship(
        &mut self,
        prepared: &gents_pack::pack::PreparedDocumentPackInstall,
    ) -> serde_json::Result<()> {
        std::iter::once(&prepared.config)
            .chain(
                prepared
                    .dependencies
                    .iter()
                    .map(|dependency| &dependency.config),
            )
            .try_for_each(|config| self.ship_config(config))
    }

    fn ship_config(&mut self, config: &gents_documents::PackConfig) -> serde_json::Result<()> {
        use gents_store::collection::Collection;
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
        Ok(())
    }

    fn protect(&self, mut failure: SessionFailure) -> SessionFailure {
        if !self.literal {
            failure.shown = None;
        }
        failure
    }

    /// An edited refusal: the documents this check can name first, then a
    /// count of the rest.
    fn edited(&self, coordinate: &str, edited: &[String]) -> SessionFailure {
        let named: Vec<String> = edited
            .iter()
            .filter(|name| self.literal && self.shipped.contains(*name))
            .cloned()
            .collect();
        let documents = gents_pack::pack::EditedDocumentRefusal {
            coordinate: coordinate.to_owned(),
            unnamed: edited.len() - named.len(),
            named,
        };
        let mut failure = SessionFailure::shown(
            PackSessionFailureClass::DocumentsEdited,
            documents.message_for_surface(gents_pack::pack::PackChangeSurface::Hosted),
            "not_installed",
        );
        failure.documents = Some(documents);
        failure
    }
}

/// A failed hosted session: the class sent, the chain kept on the runtime,
/// and whether anything may have been installed.
pub(crate) struct SessionFailure {
    pub class: PackSessionFailureClass,
    /// What the runtime may show: a typed refusal's own text, set only where
    /// the runtime holds that refusal and its class shows runtime text.
    shown: Option<String>,
    pub outcome: &'static str,
    documents: Option<gents_pack::pack::EditedDocumentRefusal>,
}

impl SessionFailure {
    /// A failure before anything was written. The chain is dropped: it can
    /// quote install values, so it is neither sent nor logged.
    pub fn not_installed(class: PackSessionFailureClass) -> impl FnOnce(anyhow::Error) -> Self {
        move |_chain| Self {
            class,
            shown: None,
            documents: None,
            outcome: "not_installed",
        }
    }

    /// A failure after the apply started, whose outcome is not confirmed.
    pub fn unconfirmed(class: PackSessionFailureClass) -> impl FnOnce(anyhow::Error) -> Self {
        move |_chain| Self {
            class,
            shown: None,
            documents: None,
            outcome: "unconfirmed",
        }
    }

    /// A typed refusal the runtime holds, shown as written. Only a class that
    /// shows runtime text carries one; any other shows its description.
    pub fn shown(class: PackSessionFailureClass, text: String, outcome: &'static str) -> Self {
        Self {
            class,
            shown: class.shows_runtime_text().then_some(text),
            documents: None,
            outcome,
        }
    }

    /// A refused pack change, matched by variant. Documents that differ and an
    /// unresolved plugin reference are shown as written: each names
    /// identifiers of the received archives (the pack, documents by collection
    /// and id, plugin coordinates and digests), and variable-derived ones use
    /// the class description instead. Edited documents are named where the
    /// received archives ship them literally and counted otherwise. `None` for
    /// any other refusal.
    pub fn change_refusal(
        error: &gents_pack::pack::PackChangeError,
        identifiers: &RefusalIdentifiers,
    ) -> Option<Self> {
        use gents_pack::pack::PackChangeError;
        let class = match error {
            PackChangeError::Edited { coordinate, edited } => {
                return Some(identifiers.edited(coordinate, edited))
            }
            PackChangeError::Differing { .. } => PackSessionFailureClass::DocumentsDiffer,
            PackChangeError::PluginReference(_) => PackSessionFailureClass::PluginUnresolved,
            _ => return None,
        };
        Some(identifiers.protect(Self::shown(
            class,
            error.message_for_surface(gents_pack::pack::PackChangeSurface::Hosted),
            "not_installed",
        )))
    }

    /// A rejected hosted transaction: documents that differ are shown as
    /// written, edited documents as `change_refusal` shows them, any other
    /// rejection by the class's description.
    pub fn rejection(
        rejection: gents_pack::pack::HostedRejection,
        identifiers: &RefusalIdentifiers,
    ) -> Self {
        use gents_pack::pack::HostedRejection;
        match rejection {
            HostedRejection::Edited { coordinate, edited } => {
                identifiers.edited(&coordinate, &edited)
            }
            differing @ HostedRejection::Differing { .. } => identifiers.protect(Self::shown(
                PackSessionFailureClass::DocumentsDiffer,
                differing.to_string(),
                "not_installed",
            )),
            HostedRejection::Other(error) => {
                Self::not_installed(PackSessionFailureClass::ApplyRejected)(error)
            }
        }
    }

    /// The `failed` message the client receives.
    pub fn message(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "failed",
            "error": self.shown.as_deref().unwrap_or_else(|| self.class.description()),
            "outcome": self.outcome,
            "class": self.class,
            "documents": self.documents,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(authored: serde_json::Value, value: &str) -> gents_documents::PackConfig {
        gents_pack::pack::decode_pack_config(
            authored,
            Some(&gents_pack::pack::PackInstallOptions {
                node_did: "did:key:test".into(),
            }),
            &|_| Some(value.into()),
            &|_, _, _| unreachable!(),
        )
        .unwrap()
    }

    fn literal_identifiers() -> RefusalIdentifiers {
        RefusalIdentifiers {
            literal: true,
            shipped: Default::default(),
        }
    }

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

    #[test]
    fn variable_derived_identifiers_never_enter_any_refusal_class() {
        use gents_pack::pack::{HostedRejection, PackChangeError, PluginReferenceUnresolved};
        use serde_json::json;
        let secret = "sk-live-b13c-identifier";
        for pointer in [
            "/tools/0/tools_id",
            "/tools/0/integrations/plugins/0/plugin",
            "/tools/0/integrations/plugins/0/digest",
        ] {
            let mut authored = json!({
                "node": {"node_did": "${GENTS_PACK_NODE_DID}"},
                "tools": [{"tools_id": "literal-tools", "integrations": {"plugins": [{
                    "plugin": "fixture/missing", "digest": "sha256:aaaa"
                }]}}]
            });
            assert!(RefusalIdentifiers::in_config(&authored));
            *authored.pointer_mut(pointer).unwrap() = json!("${B13C_IDENTIFIER}");
            let mut identifiers = RefusalIdentifiers {
                literal: RefusalIdentifiers::in_config(&authored),
                shipped: Default::default(),
            };
            assert!(!identifiers.literal, "{pointer}");
            let config = decode(authored, secret);
            identifiers.ship_config(&config).unwrap();
            let tools = &config.tools[0];
            assert!(identifiers
                .shipped
                .contains(&format!("Tools/{}", tools.tools_id)));
            let reference = &tools
                .integrations
                .as_ref()
                .unwrap()
                .plugins
                .as_ref()
                .unwrap()[0];
            let refusals = [
                PackChangeError::PluginReference(PluginReferenceUnresolved::NotInstalled {
                    pack: "fixture".into(),
                    tools: tools.tools_id.clone(),
                    plugin: reference.plugin.clone(),
                    root: "fixture/root".into(),
                }),
                PackChangeError::PluginReference(PluginReferenceUnresolved::PinMismatch {
                    pack: "fixture".into(),
                    tools: tools.tools_id.clone(),
                    plugin: reference.plugin.clone(),
                    pin: reference.digest.clone().unwrap(),
                    installed: "sha256:bbbb".into(),
                }),
                PackChangeError::Differing {
                    coordinate: "fixture/root".into(),
                    differing: vec![format!("Tools/{}", tools.tools_id)],
                },
                PackChangeError::Edited {
                    coordinate: "fixture/root".into(),
                    edited: vec![format!("Tools/{}", tools.tools_id)],
                },
            ];
            assert!(refusals
                .iter()
                .any(|refusal| refusal.to_string().contains(secret)));
            for refusal in refusals {
                for class in PackSessionFailureClass::ALL {
                    let message = identifiers
                        .protect(SessionFailure::shown(
                            class,
                            refusal.to_string(),
                            "not_installed",
                        ))
                        .message();
                    assert_eq!(
                        message["error"],
                        class.description(),
                        "{pointer}: {message}"
                    );
                    for unsafe_text in [secret, "B13C_IDENTIFIER", "${"] {
                        assert!(
                            !message.to_string().contains(unsafe_text),
                            "{pointer}: {message}"
                        );
                    }
                }
                let mut failures =
                    vec![SessionFailure::change_refusal(&refusal, &identifiers).unwrap()];
                if matches!(
                    refusal,
                    PackChangeError::Differing { .. } | PackChangeError::Edited { .. }
                ) {
                    failures.push(SessionFailure::rejection(
                        HostedRejection::of(refusal),
                        &identifiers,
                    ));
                }
                for failure in failures {
                    let message = failure.message();
                    let expected = match failure.class {
                        PackSessionFailureClass::DocumentsEdited => {
                            "1 of fixture/root's documents were edited since it was installed. \
                             overwrite replaces them with the pack's versions; keep leaves them as they are. \
                             Retry the same install with --overwrite or --keep; in the desktop choose overwrite or keep in the install review"
                        }
                        class => class.description(),
                    };
                    assert_eq!(message["error"], expected, "{pointer}: {message}");
                    for unsafe_text in [secret, "B13C_IDENTIFIER", "${"] {
                        assert!(
                            !message.to_string().contains(unsafe_text),
                            "{pointer}: {message}"
                        );
                    }
                }
            }
        }
        for text in ["${B13C_IDENTIFIER:-default}", "$${LITERAL}"] {
            assert!(!RefusalIdentifiers::in_config(
                &json!({"tools": [{"tools_id": text}]})
            ));
        }
        assert!(RefusalIdentifiers::in_config(&json!({"tools": [{
            "tools_id": "literal-tools", "display_name": "${SECRET_DISPLAY}"
        }]})));
    }

    #[test]
    fn every_refusal_class_keeps_variables_and_secrets_out_of_the_text() {
        use gents_pack::pack::{PackChangeError, PluginReferenceUnresolved};
        let tools_id = "safety-tools";
        let plugin = "private/missing";
        let secret = "sk-live-0123456789";
        let variable = "${INSTALL_TOKEN}";
        let plugin_refusals = [
            PluginReferenceUnresolved::NotInstalled {
                pack: "safety".into(),
                tools: tools_id.into(),
                plugin: plugin.into(),
                root: "gents/safety".into(),
            },
            PluginReferenceUnresolved::PinMismatch {
                pack: "safety".into(),
                tools: tools_id.into(),
                plugin: plugin.into(),
                pin: format!("sha256:{}", "a".repeat(64)),
                installed: format!("sha256:{}", "b".repeat(64)),
            },
        ];
        let refusals = plugin_refusals.map(|refusal| {
            PackChangeError::PluginReference(refusal)
                .message_for_surface(gents_pack::pack::PackChangeSurface::Hosted)
        });
        for class in PackSessionFailureClass::ALL {
            let hidden = SessionFailure::not_installed(class)(anyhow::anyhow!(
                "{variable} resolved to {secret}"
            ));
            let hidden = hidden.message().to_string();
            for unsafe_text in [secret, variable, "INSTALL_TOKEN", "${"] {
                assert!(!hidden.contains(unsafe_text), "{class:?}: {hidden}");
            }
            for refusal in &refusals {
                let message =
                    SessionFailure::shown(class, refusal.clone(), "not_installed").message();
                let text = message["error"].as_str().unwrap();
                assert_eq!(
                    text.contains(tools_id),
                    class.shows_runtime_text(),
                    "{class:?}: {text}"
                );
                assert_eq!(
                    text.contains(plugin),
                    class.shows_runtime_text(),
                    "{class:?}: {text}"
                );
            }
        }
        for (error, named) in [
            (
                PackChangeError::Edited {
                    coordinate: "gents/safety".into(),
                    edited: vec![format!("Tools/{tools_id}")],
                },
                false,
            ),
            (
                PackChangeError::Differing {
                    coordinate: "gents/safety".into(),
                    differing: vec![format!("Tools/{tools_id}")],
                },
                true,
            ),
        ] {
            let failure = SessionFailure::change_refusal(&error, &literal_identifiers()).unwrap();
            let text = failure.message().to_string();
            assert_eq!(text.contains(tools_id), named, "{text}");
        }
    }

    /// Documents that differ reach the client as written: the pack, each
    /// document by collection and id, and the overwrite or keep choice, with
    /// nothing from the chain of any other rejection.
    #[test]
    fn documents_that_differ_are_shown_by_name_and_other_rejections_by_class() {
        use gents_pack::pack::{HostedRejection, PackChangeError};
        let rejection = |value| SessionFailure::rejection(value, &literal_identifiers());
        let differing = rejection(HostedRejection::of(PackChangeError::Differing {
            coordinate: "fixture/rooted".into(),
            differing: vec!["Tools/rooted-tools".into()],
        }));
        assert_eq!(differing.class, PackSessionFailureClass::DocumentsDiffer);
        let error = differing.message()["error"].as_str().unwrap().to_owned();
        assert!(error.contains("Tools/rooted-tools"), "{error}");
        assert!(error.contains("fixture/rooted"), "{error}");
        assert!(error.contains("--overwrite or --keep"), "{error}");
        for unavailable in ["pack update", "pack remove", "--home"] {
            assert!(!error.contains(unavailable), "{error}");
        }
        // Any other rejection, even one quoting a value, sends the description.
        let other = rejection(HostedRejection::of(PackChangeError::Other(
            anyhow::anyhow!("field resolved to sk-live-0123456789"),
        )));
        assert_eq!(other.class, PackSessionFailureClass::ApplyRejected);
        assert!(!other.message().to_string().contains("sk-live"));
    }

    /// An edited refusal names the documents the received, literal archives
    /// ship, by the ids the loader rendered, and only counts the rest: one the
    /// previous installation wrote can carry an identifier rendered from an
    /// install variable, which the new archives cannot vouch for.
    #[test]
    fn an_edited_refusal_names_shipped_literal_documents_and_counts_the_rest() {
        use gents_pack::pack::{HostedRejection, PackChangeError};
        let prior = "sk-live-b13c-prior-tools";
        let authored = serde_json::json!({
            "node": {"node_did": "${GENTS_PACK_NODE_DID}"},
            "tools": [{"tools_id": "literal-tools"}, {"tools_id": "second-tools"}]
        });
        let mut identifiers = RefusalIdentifiers {
            literal: RefusalIdentifiers::in_config(&authored),
            shipped: Default::default(),
        };
        assert!(identifiers.literal);
        identifiers
            .ship_config(&decode(authored, "unused"))
            .unwrap();
        let edited = || PackChangeError::Edited {
            coordinate: "fixture/root".into(),
            edited: vec![
                format!("Tools/{prior}"),
                "Tools/literal-tools".into(),
                "Tools/stale-tools".into(),
                "Tools/second-tools".into(),
            ],
        };
        assert!(edited().to_string().contains(prior));
        for failure in [
            SessionFailure::change_refusal(&edited(), &identifiers).unwrap(),
            SessionFailure::rejection(HostedRejection::of(edited()), &identifiers),
        ] {
            assert_eq!(failure.class, PackSessionFailureClass::DocumentsEdited);
            assert_eq!(failure.outcome, "not_installed");
            let body = failure.message();
            let expected = "4 of fixture/root's documents were edited since it was installed: \
                Tools/literal-tools, Tools/second-tools and 2 others. \
                overwrite replaces them with the pack's versions; keep leaves them as they are. \
                Retry the same install with --overwrite or --keep; in the desktop choose overwrite or keep in the install review";
            assert_eq!(body["error"], expected, "{body}");
            assert_eq!(body["class"], "documents_edited", "{body}");
            for withheld in [prior, "stale-tools"] {
                assert!(!body.to_string().contains(withheld), "{body}");
            }
        }
    }

    #[test]
    fn edited_names_require_both_received_membership_and_literal_provenance() {
        use gents_pack::pack::PackChangeError;
        let mut identifiers = literal_identifiers();
        identifiers
            .ship_config(&decode(
                serde_json::json!({
                    "node": {"node_did": "${GENTS_PACK_NODE_DID}"},
                    "tools": [{"tools_id": "received"}]
                }),
                "unused",
            ))
            .unwrap();
        for (edited, literal, expected) in [
            (vec!["Tools/received"], true, ": Tools/received. overwrite"),
            (
                vec!["Tools/received", "Tools/old"],
                true,
                ": Tools/received and 1 other. overwrite",
            ),
            (vec!["Tools/old"], true, "installed. overwrite"),
            (vec!["Tools/received"], false, "installed. overwrite"),
        ] {
            identifiers.literal = literal;
            let failure = SessionFailure::change_refusal(
                &PackChangeError::Edited {
                    coordinate: "fixture/root".into(),
                    edited: edited.iter().map(|id| (*id).into()).collect(),
                },
                &identifiers,
            )
            .unwrap();
            let message = failure.message();
            let text = message["error"].as_str().unwrap();
            assert!(text.starts_with(&format!("{} of ", edited.len())), "{text}");
            assert!(text.contains(expected), "{text}");
            assert!(!text.contains("Tools/old"), "{text}");
            if !literal {
                assert!(!text.contains("Tools/received"), "{text}");
            }
        }
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
