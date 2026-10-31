use super::*;

/// Install document and plugins-only packs from the home's pack store or the registry through the
/// canonical resolver, package publication, and activation owners. The running
/// node is always the install owner; callers cannot select another DID,
/// filesystem distribution, registry endpoint, or control-plane endpoint.
pub(super) struct PackInstaller {
    pub(super) core: SelfConfigCore,
    pub(super) node: Arc<EmbeddedNode>,
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
                    let path = context
                        .resolve_existing_file(path.to_str().context("pack path is not UTF-8")?)?;
                    anyhow::ensure!(path.extension().is_some_and(|extension| extension == crate::pack::archive::EXTENSION), "native local inspection accepts a built .pack artifact; directories require a scoped build owner");
                    // The no-follow import carries the same handle the
                    // admission validated, so a swap after admission cannot
                    // change which bytes are verified and stored.
                    crate::pack::local::open(&crate::pack::local::LocalName::Path(&path), home)?
                }
            };
            // Local built artifacts flow through the same canonical
            // installers as registry-resolved packs: documents through
            // prepare/install_prepared_document_pack, plugins-only through
            // plugins_pack. Plugin authority is unchanged: preview reports
            // installation_blockers and commit refuses them unless the
            // operator grants authority, so a locally built pack (e.g. one
            // declaring child_process or bind_dir capabilities) cannot
            // silently authorize itself.
            anyhow::ensure!(
                matches!(
                    archive.manifest().metadata.kind,
                    crate::pack::PackKind::Documents | crate::pack::PackKind::Plugins
                ),
                "pack {:?} is {:?}; the model-facing installer supports document and plugins-only packs",
                archive.manifest().name,
                archive.manifest().metadata.kind
            );
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
        let access = crate::store::config_client::ConfigAccess::local(self.node.clone());
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
        let distribution = ConfigPackDistribution(resolved);
        anyhow::ensure!(
            operation == "get"
                || matches!(
                    distribution.manifest().metadata.kind,
                    crate::pack::PackKind::Documents | crate::pack::PackKind::Plugins
                ),
            "pack {:?} is {:?}; the model-facing installer supports document and plugins-only packs",
            distribution.manifest().name,
            distribution.manifest().metadata.kind
        );
        Ok(distribution)
    }

    pub(super) async fn installation_status(
        &self,
        manifest: &crate::pack::PackManifest,
    ) -> anyhow::Result<Option<Value>> {
        if manifest.metadata.kind == crate::pack::PackKind::Plugins {
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
        let access = crate::store::config_client::ConfigAccess::local(self.node.clone());
        let coordinate = format!("{}/{}", manifest.metadata.namespace, manifest.name);
        crate::pack::read_installed_pack(&access, self.core.node_did(), &coordinate)
            .await?
            .map(serde_json::to_value)
            .transpose()
            .map_err(Into::into)
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
                            false,
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
        crate::store::config_client::ConfigAccess::transact_local_write(
            &self.node,
            Some(self.core.identity()?),
            "self_config.pack_profile_choice",
            |txn| {
                Box::pin(async move {
                    for profile in bindings.values() {
                        let backend = ops::profile_backend_id(txn, owner, profile).await?;
                        ops::guard_backend_choice_in_txn(txn, owner, None, &backend).await?;
                    }
                    Ok(())
                })
            },
        )
        .await
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
        if distribution.manifest().metadata.kind == crate::pack::PackKind::Plugins {
            return self
                .plugins_pack(operation, &args, &distribution, false)
                .await;
        }
        if distribution.manifest().metadata.kind == crate::pack::PackKind::Documents {
            return self.documents(operation, &args, &distribution, false).await;
        }
        anyhow::bail!(
            "pack {:?} is {:?}; the model-facing installer supports document and plugins-only packs",
            distribution.manifest().name,
            distribution.manifest().metadata.kind
        )
    }

    pub(super) async fn apply(
        &self,
        operation: &str,
        args: PackInstallParams,
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
        if distribution.manifest().metadata.kind == crate::pack::PackKind::Plugins {
            return self
                .plugins_pack(operation, &args, &distribution, true)
                .await;
        }
        if distribution.manifest().metadata.kind == crate::pack::PackKind::Documents {
            return self.documents(operation, &args, &distribution, true).await;
        }
        anyhow::bail!(
            "pack {:?} is {:?}; the model-facing installer supports document and plugins-only packs",
            distribution.manifest().name,
            distribution.manifest().metadata.kind
        )
    }
}
