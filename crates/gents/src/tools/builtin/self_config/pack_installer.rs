use super::*;
use crate::pack::{authoring, PackKind};
use crate::plugin::authority;

/// Install document and plugins-only packs from the home's pack store or the registry through the
/// canonical resolver, package publication, and activation owners. The running
/// node is always the install owner; callers cannot select another DID,
/// filesystem distribution, registry endpoint, or control-plane endpoint.
pub(super) struct PackInstaller {
    pub(super) core: SelfConfigCore,
    pub(super) node: Arc<EmbeddedNode>,
    pub(super) actor: Option<Arc<dyn NodeIdentity>>,
    /// `None` resolves from the registry in memory and installs no plugins.
    pub(super) home: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone)]
pub(super) struct PackInstallParams {
    pub package: String,
    pub variables: BTreeMap<String, String>,
    pub inference_slots: crate::pack::PackInferenceBindings,
    pub expected_digest: Option<String>,
}

/// The pack kinds this installer installs: document and plugins-only packs.
pub(super) struct ConfigPackDistribution(pub(super) crate::pack::resolve::ResolvedNamedPack);

impl ConfigPackDistribution {
    pub(super) fn manifest(&self) -> &crate::pack::PackManifest {
        self.0.archive.manifest()
    }

    pub(super) fn digest(&self) -> &str {
        self.0.archive.digest()
    }

    pub(super) fn source(&self) -> &'static str {
        match self.0.from {
            crate::pack::resolve::ResolvedFrom::Installed => "installed",
            crate::pack::resolve::ResolvedFrom::Store => "store",
            crate::pack::resolve::ResolvedFrom::Registry { .. } => "registry",
        }
    }

    /// Pin the preview's resolution so applying it resolves the same pack
    /// again rather than whatever is newest by then.
    pub(super) fn pinned_spec(&self, requested: &str) -> String {
        if crate::pack::local::classify(requested).is_some() {
            return self.digest().to_owned();
        }
        let manifest = self.manifest();
        if crate::pack::store::is_valid_index_version(&manifest.version) {
            format!(
                "{}/{}@{}",
                manifest.metadata.namespace, manifest.name, manifest.version
            )
        } else {
            requested.to_owned()
        }
    }

    pub(super) fn registry_artifact_digest(&self) -> Option<&str> {
        match &self.0.from {
            crate::pack::resolve::ResolvedFrom::Registry {
                artifact_digest, ..
            } => Some(artifact_digest),
            _ => None,
        }
    }
}

impl PackInstaller {
    pub(super) fn validate(&self, args: &PackInstallParams) -> anyhow::Result<()> {
        let coordinate = args.package.trim();
        anyhow::ensure!(!coordinate.is_empty(), "pack name must not be blank");
        if crate::pack::local::classify(coordinate).is_none() {
            crate::pack::resolve::parse_pack_spec(coordinate)?;
        }
        anyhow::ensure!(
            args.variables.len() <= 32,
            "pack variables exceed the 32-entry limit"
        );
        for (name, value) in &args.variables {
            anyhow::ensure!(
                name != "GENTS_PACK_NODE_DID"
                    && !name.is_empty()
                    && name.len() <= 128
                    && name.bytes().all(|byte| {
                        byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_'
                    }),
                "invalid or protected pack variable name {name:?}"
            );
            anyhow::ensure!(
                !name.ends_with("_MODEL") && !name.ends_with("_ENDPOINT"),
                "inference variable {name:?} is unsupported; bind declared roles with --inference-slot NAME=PROFILE_ID"
            );
            anyhow::ensure!(value.len() <= 4096, "pack variable {name:?} is too large");
        }
        anyhow::ensure!(
            args.inference_slots.len() <= 32,
            "pack inference slot bindings exceed the 32-entry limit"
        );
        Ok(())
    }

    pub(super) async fn resolve(
        &self,
        operation: &str,
        args: &PackInstallParams,
    ) -> anyhow::Result<ConfigPackDistribution> {
        self.validate(args)?;
        let coordinate = args.package.trim();
        if let Some(local) = crate::pack::local::classify(coordinate) {
            let home = self
                .home
                .as_deref()
                .context("local packs require a runtime plugin home")?;
            let archive = match local {
                crate::pack::local::LocalName::Digest(_) => crate::pack::local::open(&local, home)?,
                crate::pack::local::LocalName::Path(path) => {
                    // Effective authority, not the process ceiling alone: a
                    // narrower agent selection (or WorkspaceRoot policy) must
                    // bound what this inspection can read.
                    let effective = self
                        .core
                        .read_effective_config(&BTreeSet::new(), false, false)
                        .await?;
                    let mode = effective
                        .pointer("/runtime_effective/effective/file_mode")
                        .and_then(Value::as_str)
                        .map(crate::tools::surface::FileToolMode::parse)
                        .transpose()?
                        .unwrap_or_default();
                    anyhow::ensure!(mode != crate::tools::surface::FileToolMode::Off, "local pack inspection requires effective file read permission on the invoking agent");
                    let root = effective
                        .pointer("/runtime_effective/effective/root")
                        .and_then(Value::as_str)
                        .context("local pack inspection requires an effective tool root")?;
                    let context = crate::tools::native::ToolContext::new(root.into(), false)?;
                    let requested = path.to_str().context("pack path is not UTF-8")?;
                    let path = context.resolve_path(requested)?;
                    if path.is_dir() {
                        // A directory install packs what is on disk, and a
                        // present artifact may predate its source, so plugin
                        // source installs as the pack a build wrote.
                        let manifest = authoring::read_manifest(&path)?;
                        let sources = authoring::plugins_with_source(&manifest);
                        if !sources.is_empty() {
                            let built = authoring::built_pack_path(&path, &manifest);
                            let (step, next_call) = if built.is_file() {
                                let built = context.display_path(&built);
                                (
                                    format!("install the pack it was built into, {built}; build again first if the source changed since"),
                                    json!({"argv":["pack","preview","install",built]}),
                                )
                            } else {
                                (
                                    "build it, then preview the .pack the build names".to_owned(),
                                    json!({"argv":["pack","build",context.display_path(&path)]}),
                                )
                            };
                            return Err(command::CommandGuidance {
                                message: format!(
                                    "{requested} has plugin source ({}), which a directory install does not compile: {step}",
                                    sources.join(", ")
                                ),
                                next_call,
                            }
                            .into());
                        }
                    } else {
                        anyhow::ensure!(
                            path.is_file()
                                && path.extension().is_some_and(
                                    |extension| extension == crate::pack::archive::EXTENSION
                                ),
                            "{requested} is neither a pack directory nor a built .pack file"
                        );
                    }
                    // The no-follow import carries the same handle the
                    // admission validated, so a swap after admission cannot
                    // change which bytes are verified and stored.
                    crate::pack::local::open(&crate::pack::local::LocalName::Path(&path), home)?
                }
            };
            // Local built artifacts flow through the same canonical
            // installers as registry-resolved packs: documents through
            // prepare/install_prepared_document_pack, plugins and assets
            // packs through plugins_pack. Plugin authority follows `grants_authority`:
            // without it preview reports installation_blockers and commit
            // refuses them; with it the install grants what the pack asks
            // for and the receipt lists it.
            return Ok(ConfigPackDistribution(
                crate::pack::resolve::ResolvedNamedPack {
                    archive,
                    from: crate::pack::resolve::ResolvedFrom::Store,
                },
            ));
        }
        let registry_url = crate::pack::registry::resolve_registry_url(None);
        let spec = match crate::pack::resolve::parse_pack_spec(coordinate)? {
            parsed if operation == "update" && parsed.version.is_none() => {
                let client = crate::pack::registry::RegistryClient::new(registry_url.clone());
                let latest = crate::pack::registry::resolve_pack_coordinate(
                    &client,
                    parsed.namespace,
                    parsed.name,
                    None,
                )
                .await
                .with_context(|| {
                    format!(
                        "the newest version of {coordinate} could not be looked up on the registry at {registry_url}"
                    )
                })?;
                format!("{}/{}@{}", parsed.namespace, parsed.name, latest.version)
            }
            _ => coordinate.to_owned(),
        };
        let access = self.core.access()?;
        let installed = crate::pack::installed_packs(
            self.home.as_deref(),
            Some((&access, self.core.node_did())),
        )
        .await?;
        let resolved = crate::pack::resolve::resolve_named(
            &spec,
            &crate::pack::resolve::ResolveOptions {
                home: self.home.as_deref(),
                registry_url,
                installed: &installed,
            },
        )
        .await?;
        Ok(ConfigPackDistribution(resolved))
    }

    pub(super) async fn installation_status(
        &self,
        manifest: &crate::pack::PackManifest,
    ) -> anyhow::Result<Option<Value>> {
        if manifest.metadata.kind != PackKind::Documents {
            return self
                .home
                .as_deref()
                .map(|home| {
                    crate::pack::read_home_install(
                        home,
                        &format!("{}/{}", manifest.metadata.namespace, manifest.name),
                    )
                })
                .transpose()?
                .flatten()
                .map(serde_json::to_value)
                .transpose()
                .map_err(Into::into);
        }
        let access = self.core.access()?;
        let coordinate = format!("{}/{}", manifest.metadata.namespace, manifest.name);
        crate::pack::read_installed_pack(&access, self.core.node_did(), &coordinate)
            .await?
            .map(serde_json::to_value)
            .transpose()
            .map_err(Into::into)
    }

    /// Whether this agent's installs carry the operator's consent to what a
    /// pack's plugins ask for (`self_config.enable_pack_authority`).
    pub(super) fn grants_authority(&self) -> bool {
        self.core.held_grants().pack_authority
    }

    /// What installing `manifests` grants each plugin that asks for anything:
    /// a successful install grants exactly what the plugin declares.
    pub(super) fn granted_authority<'a>(
        &self,
        manifests: impl IntoIterator<Item = &'a crate::pack::PackManifest>,
    ) -> anyhow::Result<Vec<Value>> {
        let mut granted = Vec::new();
        for manifest in manifests {
            for plugin in &manifest.metadata.plugins {
                let declared = authority::declared_manifold(plugin)?;
                if let Some(description) = authority::describe_plugin_authority(plugin, &declared) {
                    granted.push(json!({
                        "plugin": format!("{}/{}", manifest.metadata.namespace, plugin.name),
                        "granted": description,
                        "manifold": plugin.manifold,
                    }));
                }
            }
        }
        Ok(granted)
    }

    pub(super) fn plugin_install_blockers(
        &self,
        manifest: &crate::pack::PackManifest,
    ) -> Vec<Value> {
        manifest
            .metadata
            .plugins
            .iter()
            .filter_map(|plugin| {
                let result = self
                    .home
                    .as_deref()
                    .context("this runtime has no plugin home")
                    .and_then(|home| {
                        crate::plugin::store::grant_on_install(
                            home,
                            &manifest.metadata.namespace,
                            plugin,
                            self.grants_authority(),
                        )
                    });
                result
                    .err()
                    .map(|error| json!({"plugin":plugin.name,"reason":format!("{error:#}")}))
            })
            .collect()
    }

    /// Installs the plugins `distribution` ships into the home's plugin store,
    /// sealed: a plugin that asks for authority is refused, because granting
    /// it is the operator's call. Local plugin records are the publication
    /// boundary; later failures do not roll them back.
    /// Pack slots have no current backend: each bound profile must be
    /// account-free or on its provider's default account.
    pub(super) async fn fence_bindings(
        &self,
        bindings: &crate::pack::PackInferenceBindings,
    ) -> anyhow::Result<()> {
        let owner = self.core.node_did();
        self.core
            .access()?
            .transact_write("self_config.pack_profile_choice", |txn| {
                Box::pin(async move {
                    for profile in bindings.values() {
                        let backend = ops::profile_backend_id(txn, owner, profile).await?;
                        ops::guard_backend_choice_in_txn(txn, owner, None, &backend).await?;
                    }
                    Ok(())
                })
            })
            .await
    }

    /// What a preview says about documents edited since the install: which
    /// they are and what each `options.edited` choice does to them. Absent
    /// when nothing was edited.
    pub(super) async fn edited_documents(
        &self,
        coordinate: &str,
        removal: bool,
    ) -> anyhow::Result<Option<Value>> {
        let edited = crate::pack::edited_installed_documents(
            &self.core.access()?,
            self.core.node_did(),
            coordinate,
            removal,
        )
        .await?;
        Ok((!edited.is_empty()).then(|| {
            json!({
                "documents": edited,
                "decide_with": "options.edited",
                "overwrite": if removal { "deletes them with the rest of the pack" } else { "replaces them with the pack's versions; the live edits are lost" },
                "keep": if removal { "leaves them on the node; the rest of the pack is removed" } else { "leaves them as edited; the rest of the pack is installed" },
            })
        }))
    }

    /// The selections whose pins refuse the plugins `distribution` publishes.
    pub(super) async fn pin_conflicts(
        &self,
        distribution: &ConfigPackDistribution,
    ) -> anyhow::Result<Vec<crate::pack::PluginPinConflict>> {
        let manifest = distribution.manifest();
        let mut replacements = BTreeMap::new();
        for plugin in &manifest.metadata.plugins {
            replacements.insert(
                format!("{}/{}", manifest.metadata.namespace, plugin.name),
                format!(
                    "sha256:{}",
                    crate::plugin::store::content_digest_hex(
                        distribution.0.archive.asset(&plugin.artifact)?
                    )
                ),
            );
        }
        crate::pack::plugin_pin_conflicts(&self.core.access()?, self.core.node_did(), &replacements)
            .await
    }

    /// Refuses an install that replaces a plugin some Tools pins to another
    /// digest, as the Tools edit that moves the first such pin.
    async fn require_pins_follow(
        &self,
        distribution: &ConfigPackDistribution,
    ) -> anyhow::Result<()> {
        let conflicts = self.pin_conflicts(distribution).await?;
        let Some(first) = conflicts.first() else {
            return Ok(());
        };
        let others = conflicts
            .iter()
            .skip(1)
            .filter(|conflict| conflict.tools != first.tools)
            .map(|conflict| format!("Tools {:?} of agent {}", conflict.tools, conflict.agent))
            .collect::<BTreeSet<_>>();
        Err(command::CommandGuidance {
            message: format!(
                "nothing was installed: Tools {:?} of agent {} pins {} at {}, and this install replaces it with {}. Move the pin with the tools update below, then repeat this call.{}",
                first.tools,
                first.agent,
                first.plugin,
                first.pin,
                first.replacement,
                if others.is_empty() {
                    String::new()
                } else {
                    format!(
                        " The same is then asked for {}.",
                        others.into_iter().collect::<Vec<_>>().join(", ")
                    )
                }
            ),
            next_call: pin_move_call(first),
        }
        .into())
    }

    /// Refuses a change that would meet edited documents without a decision,
    /// as the same call with the decision that loses nothing; the message
    /// names the other one. `words` is the pack command as it was called.
    pub(super) async fn require_edited_decision(
        &self,
        coordinate: &str,
        removal: bool,
        edited: crate::pack::DriftPolicy,
        words: &[String],
    ) -> anyhow::Result<()> {
        if edited != crate::pack::DriftPolicy::Refuse {
            return Ok(());
        }
        let Some(found) = self.edited_documents(coordinate, removal).await? else {
            return Ok(());
        };
        let call = |decision: &str| {
            let mut argv = vec!["pack".to_owned()];
            argv.extend(words.iter().cloned());
            json!({"argv": argv, "options": {"edited": decision}})
        };
        let documents = found["documents"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        Err(command::CommandGuidance {
            message: format!(
                "nothing was changed: {documents} of {coordinate} were edited since it was installed. Repeat the call with options.edited: \"keep\" {}; \"overwrite\" {}. Overwrite: {}",
                found["keep"].as_str().unwrap_or_default(),
                found["overwrite"].as_str().unwrap_or_default(),
                call("overwrite")
            ),
            next_call: call("keep"),
        }
        .into())
    }

    pub(super) async fn preview(
        &self,
        operation: &str,
        args: PackInstallParams,
    ) -> anyhow::Result<String> {
        let distribution = self.resolve(operation, &args).await?;
        if let Some(expected) = args.expected_digest.as_deref() {
            anyhow::ensure!(
                expected == distribution.digest(),
                "pack digest changed: requested {expected:?}, resolved {:?}; preview again",
                distribution.digest()
            );
        }
        match distribution.manifest().metadata.kind {
            PackKind::Documents => {
                self.documents(operation, &args, &distribution, false, Default::default())
                    .await
            }
            PackKind::Plugins | PackKind::Assets => {
                self.plugins_pack(operation, &args, &distribution, false)
                    .await
            }
        }
    }

    pub(super) async fn apply(
        &self,
        operation: &str,
        args: PackInstallParams,
        edited: crate::pack::DriftPolicy,
        words: &[String],
    ) -> anyhow::Result<String> {
        let distribution = self.resolve(operation, &args).await?;
        let expected = args.expected_digest.as_deref().context(
            "pack install/update requires --digest from config pack preview; preview pins the exact artifact being authorized",
        )?;
        anyhow::ensure!(
            expected == distribution.digest(),
            "pack digest changed: preview authorized {expected:?}, resolved {:?}; preview again",
            distribution.digest()
        );
        self.require_pins_follow(&distribution).await?;
        match distribution.manifest().metadata.kind {
            PackKind::Documents => {
                let manifest = distribution.manifest();
                self.require_edited_decision(
                    &format!("{}/{}", manifest.metadata.namespace, manifest.name),
                    false,
                    edited,
                    words,
                )
                .await?;
                self.documents(operation, &args, &distribution, true, edited)
                    .await
            }
            PackKind::Plugins | PackKind::Assets => {
                self.plugins_pack(operation, &args, &distribution, true)
                    .await
            }
        }
    }
}

/// The `tools update` that moves `conflict`'s pins to the replacement. The
/// whole `integrations` field is sent because `set` replaces a field.
pub(super) fn pin_move_call(conflict: &crate::pack::PluginPinConflict) -> Value {
    json!({
        "argv": ["tools", "update"],
        "options": {"agent": conflict.agent, "shared": "acknowledge"},
        "set": {"integrations": conflict.integrations},
    })
}
