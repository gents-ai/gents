use super::*;

impl PackInstaller {
    pub(super) async fn documents(
        &self,
        operation: &str,
        args: &PackInstallParams,
        distribution: &ConfigPackDistribution,
        commit: bool,
    ) -> anyhow::Result<String> {
        let access = crate::config_client::ConfigAccess::local(self.node.clone());
        let owner = self.core.node_did();
        let prior = self.installation_status(distribution.manifest()).await?;
        anyhow::ensure!(
            operation != "update" || prior.is_some(),
            "pack {:?} is not installed; use pack install",
            distribution.manifest().name
        );
        let mut dependencies = Vec::new();
        for coordinate in &distribution.manifest().metadata.dependencies {
            dependencies.push(
                self.resolve(
                    "get",
                    &PackInstallParams {
                        package: coordinate.clone(),
                        variables: args.variables.clone(),
                        inference_slots: BTreeMap::new(),
                        expected_digest: None,
                    },
                )
                .await?,
            );
        }
        let mut blockers = self.plugin_install_blockers(distribution.manifest());
        for dependency in &dependencies {
            blockers.extend(self.plugin_install_blockers(dependency.manifest()));
        }
        for slot in args.inference_slots.keys() {
            anyhow::ensure!(
                std::iter::once(distribution.manifest())
                    .chain(dependencies.iter().map(|d| d.manifest()))
                    .any(|manifest| manifest
                        .metadata
                        .inference_slots
                        .iter()
                        .any(|declared| &declared.name == slot)),
                "pack and its dependencies have no inference slot {slot:?}"
            );
        }
        let mut missing_slots = std::collections::BTreeSet::new();
        let mut inspected = Vec::new();
        for manifest in std::iter::once(distribution.manifest())
            .chain(dependencies.iter().map(|d| d.manifest()))
        {
            let requested = args
                .inference_slots
                .iter()
                .filter(|(slot, _)| {
                    manifest
                        .metadata
                        .inference_slots
                        .iter()
                        .any(|declared| &declared.name == *slot)
                })
                .map(|(slot, profile)| (slot.clone(), profile.clone()))
                .collect();
            let preview =
                crate::pack::inspect_pack_inference_bindings(&access, manifest, owner, &requested)
                    .await?;
            for slot in &preview.slots {
                if !slot.optional && !args.inference_slots.contains_key(&slot.name) {
                    missing_slots.insert(slot.name.clone());
                }
            }
            inspected.push(preview);
        }
        if !commit && !missing_slots.is_empty() {
            // Incomplete previews still must reject unauthorized supplied bindings.
            // Complete previews and installs retain the prepared-binding fence below.
            for preview in &inspected {
                self.fence_bindings(&preview.bindings).await?;
            }
            return Ok(serde_json::to_string_pretty(&json!({
                "committed": false, "ready": false, "operation": operation,
                "package": distribution.manifest().name, "version": distribution.manifest().version,
                "source": distribution.source(), "artifact_digest": distribution.digest(),
                "inference": inspected, "missing_inference_slots": missing_slots, "installed": prior,
                "next": "repeat --inference-slot NAME=PROFILE_ID for every missing slot, using exact eligible profile IDs",
            }))?);
        }
        let scope = crate::pack::PackInstallOptions {
            node_did: owner.to_owned(),
        };
        let environment = |name: &str| args.variables.get(name).cloned();
        let authored = crate::pack::load_pack_config(
            distribution.manifest(),
            &scope,
            &|path| {
                distribution
                    .0
                    .archive
                    .asset(path)
                    .map(|asset| asset.to_vec())
            },
            &environment,
        )?;
        let prepared = crate::pack::prepare_document_pack_install(
            &access,
            owner,
            distribution.manifest(),
            &authored,
            &args.inference_slots,
            &dependencies
                .iter()
                .map(|d| &d.0.archive)
                .collect::<Vec<_>>(),
            crate::pack::InstalledVia::SelfConfig,
            self.home.as_deref(),
            Some(&distribution.0.archive),
            &environment,
        )
        .await?;
        // Inference slot bindings pick provider accounts exactly like the
        // install lifecycle: every selected profile must be account-free or on
        // its provider's default account. Fence before the preview return or
        // any installation side effect so a documents pack cannot publish
        // records or plugins through a binding an install would refuse.
        self.fence_bindings(&prepared.inference.bindings).await?;
        for dependency in &prepared.dependencies {
            self.fence_bindings(&dependency.inference.bindings).await?;
        }
        let mut schemas = Vec::new();
        for path in crate::pack::document_pack_schema_paths(distribution.manifest())? {
            let sdl = std::str::from_utf8(distribution.0.archive.asset(path)?)?;
            schemas.push(crate::config_client::preview_schema_install(&access, sdl).await?);
        }
        let dependency_receipts = dependencies.iter().map(|dependency| json!({
            "coordinate": format!("{}/{}", dependency.manifest().metadata.namespace, dependency.manifest().name),
            "version": dependency.manifest().version, "artifact_digest": dependency.digest(),
        })).collect::<Vec<_>>();
        if !commit {
            return Ok(serde_json::to_string_pretty(&json!({
                "committed": false, "ready": blockers.is_empty(), "operation": operation,
                "package": distribution.manifest().name, "version": distribution.manifest().version,
                "source": distribution.source(), "artifact_digest": distribution.digest(),
                "registry_artifact_digest": distribution.registry_artifact_digest(),
                "inference": prepared.inference, "dependency_inference": prepared.dependency_inference,
                "dependencies": dependency_receipts, "schemas": schemas, "installation_blockers": blockers,
                "installed": prior,
                "materialized_ids": crate::pack::pack_document_digests(&prepared.config)?.into_keys().map(|(collection, id)| json!({"collection":collection,"id":id})).collect::<Vec<_>>(),
                "apply_with": blockers.is_empty().then(|| json!({
                    "argv_prefix": ["pack", operation, distribution.pinned_spec(&args.package), "--digest", distribution.digest()],
                    "repeat_inference_slots": args.inference_slots, "repeat_variables": args.variables,
                })),
            }))?);
        }
        anyhow::ensure!(
            blockers.is_empty(),
            "pack plugins require operator authorization: {blockers:?}"
        );
        let receipt = crate::pack::install_prepared_document_pack(
            &access,
            owner,
            &distribution.0.archive,
            &dependencies
                .iter()
                .map(|d| &d.0.archive)
                .collect::<Vec<_>>(),
            &prepared,
            self.home.as_deref(),
            false,
            crate::pack::InstalledVia::SelfConfig,
            crate::pack::DriftPolicy::Refuse,
        )
        .await?;
        let effective = self
            .installation_status(distribution.manifest())
            .await?
            .context("installed document pack is not discoverable")?;
        anyhow::ensure!(
            effective["digest"].as_str() == Some(distribution.digest()),
            "installed document pack digest does not match requested artifact"
        );
        Ok(serde_json::to_string_pretty(&json!({
            "operation": operation, "source": distribution.source(), "artifact_digest": distribution.digest(),
            "registry_artifact_digest": distribution.registry_artifact_digest(), "install": receipt,
            "inference": prepared.inference, "dependency_inference": prepared.dependency_inference,
            "dependencies": dependency_receipts, "effective": effective,
            "effect": "The document pack is installed.",
        }))?)
    }
}
