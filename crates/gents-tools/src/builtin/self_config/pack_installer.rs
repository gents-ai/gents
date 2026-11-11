use super::*;

/// The config tool's pack commands: it admits what the calling agent names,
/// asks the node's pack owner ([`NodePacks`]) for each decision and renders
/// the answer with the call to make next.
pub struct PackInstaller {
    pub core: SelfConfigCore,
    pub node: Arc<EmbeddedNode>,
    pub actor: Option<Arc<dyn NodeIdentity>>,
    /// `None` resolves from the registry in memory and installs no plugins.
    pub home: Option<std::path::PathBuf>,
}

impl PackInstaller {
    /// This agent's pack operations on its node, through `access`.
    pub fn packs<'a>(
        &'a self,
        access: &'a gents_store::config_client::ConfigAccess,
    ) -> NodePacks<'a> {
        NodePacks {
            access,
            node_did: self.core.node_did(),
            home: self.home.as_deref(),
            grants_authority: self.core.held_grants().pack_authority,
        }
    }

    pub async fn resolve(
        &self,
        operation: &str,
        args: &PackChange,
    ) -> anyhow::Result<Distribution> {
        args.validate()?;
        let access = self.core.access()?;
        let packs = self.packs(&access);
        let Some(requested_path) = args.local_path() else {
            return packs
                .resolve_named(operation, args)
                .await
                .map_err(|error| match error {
                    LocalPackError::NotStored(digest) => command::CommandGuidance {
                        message: format!(
                            "{digest} is not in this node's pack store. A digest opens only a pack the store already holds: one that is installed, or was previewed from its file. A pack that pack build just wrote is a file: name the .pack path the build returned as `out`, spelled ./FILE.pack, in pack preview install or pack preview update. pack list shows what the store holds"
                        ),
                        next_call: json!({"argv":["pack","list"]}),
                    }
                    .into(),
                    other => anyhow::Error::from(other),
                });
        };
        anyhow::ensure!(
            self.home.is_some(),
            "local packs require a runtime plugin home"
        );
        // Effective authority, not the process ceiling alone: a narrower
        // agent selection (or WorkspaceRoot policy) must bound what this
        // inspection can read.
        let effective = self
            .core
            .read_effective_config(&BTreeSet::new(), false, false)
            .await?;
        let mode = effective
            .pointer("/runtime_effective/effective/file_mode")
            .and_then(Value::as_str)
            .map(gents_documents::FileToolMode::parse)
            .transpose()?
            .unwrap_or_default();
        anyhow::ensure!(
            mode != gents_documents::FileToolMode::Off,
            "local pack inspection requires effective file read permission on the invoking agent"
        );
        let root = effective
            .pointer("/runtime_effective/effective/root")
            .and_then(Value::as_str)
            .context("local pack inspection requires an effective tool root")?;
        let context = crate::native::ToolContext::new(root.into(), false)?;
        let requested = requested_path.to_str().context("pack path is not UTF-8")?;
        let path = context.resolve_path(requested)?;
        // Local built artifacts flow through the same installers as
        // registry-resolved packs. Plugin authority follows the agent's pack
        // authority grant: without it preview reports installation_blockers
        // and commit refuses them; with it the install grants what the pack
        // asks for and the receipt lists it.
        packs
            .open_path(requested_path, &path)
            .map_err(|error| match error {
                LocalPackError::PluginSource { sources, built } => {
                    let (step, next_call) = match built {
                        Some(built) => {
                            let built = context.display_path(&built);
                            (
                                format!("install the pack it was built into, {built}; build again first if the source changed since"),
                                json!({"argv":["pack","preview","install",built]}),
                            )
                        }
                        None => (
                            "build it, then preview the .pack the build names".to_owned(),
                            json!({"argv":["pack","build",context.display_path(&path)]}),
                        ),
                    };
                    command::CommandGuidance {
                        message: format!(
                            "{requested} has plugin source ({}), which a directory install does not compile: {step}",
                            sources.join(", ")
                        ),
                        next_call,
                    }
                    .into()
                }
                other => anyhow::Error::from(other),
            })
    }

    /// Pack slots have no current backend: each bound profile must be
    /// account-free or on its provider's default account.
    pub async fn fence_bindings(
        &self,
        bindings: &BTreeMap<String, String>,
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
    pub async fn edited_documents(
        &self,
        coordinate: &str,
        removal: bool,
    ) -> anyhow::Result<Option<Value>> {
        let access = self.core.access()?;
        let edited = self
            .packs(&access)
            .edited_documents(coordinate, removal)
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
    pub async fn pin_conflicts(
        &self,
        distribution: &Distribution,
    ) -> anyhow::Result<Vec<PluginPinConflict>> {
        let access = self.core.access()?;
        self.packs(&access).pin_conflicts(distribution).await
    }

    /// Refuses an install that replaces a plugin some Tools pins to another
    /// digest, as the Tools edit that moves the first such pin.
    async fn require_pins_follow(&self, distribution: &Distribution) -> anyhow::Result<()> {
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
    pub async fn require_edited_decision(
        &self,
        coordinate: &str,
        removal: bool,
        edited: DriftPolicy,
        words: &[String],
    ) -> anyhow::Result<()> {
        if edited != DriftPolicy::Refuse {
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

    pub async fn preview(
        &self,
        operation: &str,
        args: PackChange,
    ) -> anyhow::Result<String> {
        let distribution = self.resolve(operation, &args).await?;
        if let Some(expected) = args.expected_digest.as_deref() {
            anyhow::ensure!(
                expected == distribution.digest(),
                "pack digest changed: requested {expected:?}, resolved {:?}; preview again",
                distribution.digest()
            );
        }
        if distribution.is_documents() {
            self.documents(operation, &args, &distribution, false, Default::default())
                .await
        } else {
            self.plugins_pack(operation, &args, &distribution, false)
                .await
        }
    }

    pub async fn apply(
        &self,
        operation: &str,
        args: PackChange,
        edited: Option<&str>,
        words: &[String],
    ) -> anyhow::Result<String> {
        let edited = node_packs::edited_policy(edited)?;
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
        if distribution.is_documents() {
            self.require_edited_decision(&distribution.coordinate(), false, edited, words)
                .await?;
            self.documents(operation, &args, &distribution, true, edited)
                .await
        } else {
            self.plugins_pack(operation, &args, &distribution, true)
                .await
        }
    }
}

/// The Tools edit that sets `conflict`'s selecting Tools to `integrations`.
pub fn pin_edit(conflict: &PluginPinConflict, integrations: &Value) -> Vec<String> {
    vec![
        "tools".to_owned(),
        "edit".to_owned(),
        "--agent".to_owned(),
        conflict.agent.clone(),
        "--shared".to_owned(),
        "acknowledge".to_owned(),
        "--set".to_owned(),
        format!("integrations={integrations}"),
    ]
}

/// The `tools update` that moves `conflict`'s pins to the replacement. The
/// whole `integrations` field is sent because `set` replaces a field.
pub fn pin_move_call(conflict: &PluginPinConflict) -> Value {
    json!({
        "argv": ["tools", "update"],
        "options": {"agent": conflict.agent, "shared": "acknowledge"},
        "set": {"integrations": conflict.integrations},
    })
}
