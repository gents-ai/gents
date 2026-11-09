use super::*;

impl PackInstaller {
    pub(super) async fn documents(
        &self,
        operation: &str,
        args: &PackInstallParams,
        distribution: &ConfigPackDistribution,
        commit: bool,
    ) -> anyhow::Result<String> {
        let access = self.core.access()?;
        let node_binding = self.core.refresh_node_binding().await?;
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
            let preview = crate::pack::inspect_pack_inference_bindings_with_node_binding(
                &access,
                manifest,
                owner,
                &requested,
                node_binding.as_ref(),
            )
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
            return ordered! {
                "ready": false, "committed": false,
                "missing_inference_slots": missing_slots,
                "next": "repeat --inference-slot NAME=PROFILE_ID for every missing slot, using exact eligible profile IDs",
                "inference": without_nulls(serde_json::to_value(&inspected)?),
                "operation": operation,
                "package": distribution.manifest().name, "version": distribution.manifest().version,
                "installed": prior,
                "source": distribution.source(), "artifact_digest": distribution.digest(),
            }
            .pretty();
        }
        let environment = |name: &str| args.variables.get(name).cloned();
        let provisioner = match (self.core.runtime_identity().ok(), self.actor.as_deref()) {
            (Some(runtime), Some(actor)) => Some(crate::pack::AgentProvisioner {
                node: &self.node,
                runtime,
                actor: Some(actor),
                selected_agent_did: Some(self.core.agent_did()),
                node_binding: node_binding.as_ref(),
            }),
            _ => None,
        };
        let prepared = crate::pack::prepare_document_pack_archive_with_provisioner(
            &access,
            owner,
            &distribution.0.archive,
            &args.inference_slots,
            &dependencies
                .iter()
                .map(|d| &d.0.archive)
                .collect::<Vec<_>>(),
            crate::pack::InstalledVia::SelfConfig,
            self.grants_authority(),
            self.home.as_deref(),
            &environment,
            provisioner.as_ref(),
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
            schemas.push(crate::store::config_client::preview_schema_install(&access, sdl).await?);
        }
        let authority = self.granted_authority(
            std::iter::once(distribution.manifest())
                .chain(dependencies.iter().map(|d| d.manifest())),
        )?;
        let dependency_receipts = dependencies.iter().map(|dependency| json!({
            "coordinate": format!("{}/{}", dependency.manifest().metadata.namespace, dependency.manifest().name),
            "version": dependency.manifest().version, "artifact_digest": dependency.digest(),
        })).collect::<Vec<_>>();
        if !commit {
            let mut materialized = BTreeMap::<String, Vec<String>>::new();
            for (collection, id) in
                crate::pack::pack_document_digests(&prepared.config)?.into_keys()
            {
                materialized
                    .entry(collection.to_string())
                    .or_default()
                    .push(id.to_string());
            }
            return ordered! {
                "ready": blockers.is_empty(), "committed": false,
                "apply_with": blockers.is_empty().then(|| json!({
                    "argv_prefix": ["pack", operation, distribution.pinned_spec(&args.package), "--digest", distribution.digest()],
                    "repeat_inference_slots": args.inference_slots, "repeat_variables": args.variables,
                })),
                "installation_blockers": blockers,
                "would_grant": authority,
                "operation": operation,
                "package": distribution.manifest().name, "version": distribution.manifest().version,
                "installed": prior,
                "inference": bound_inference(serde_json::to_value(&prepared.inference)?),
                "dependency_inference": (!prepared.dependency_inference.is_empty()).then_some(&prepared.dependency_inference),
                "dependencies": (!dependency_receipts.is_empty()).then_some(&dependency_receipts),
                "schemas": schemas.iter().map(schema_summary).collect::<anyhow::Result<Vec<_>>>()?,
                "materialized_ids": materialized,
                "source": distribution.source(), "artifact_digest": distribution.digest(),
                "registry_artifact_digest": distribution.registry_artifact_digest(),
            }
            .pretty();
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
            self.grants_authority(),
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
        ordered! {
            "effect": "The document pack is installed.",
            "effective": effective,
            "install": without_empty_lists(serde_json::to_value(&receipt)?),
            "granted_authority": authority,
            "operation": operation,
            "inference": bound_inference(serde_json::to_value(&prepared.inference)?),
            "dependency_inference": (!prepared.dependency_inference.is_empty()).then_some(&prepared.dependency_inference),
            "dependencies": (!dependency_receipts.is_empty()).then_some(&dependency_receipts),
            "source": distribution.source(), "artifact_digest": distribution.digest(),
            "registry_artifact_digest": distribution.registry_artifact_digest(),
        }
        .pretty()
    }
}

/// Every slot is bound, so the receipt keeps the bindings and slots and leaves
/// the eligible-profile inventory to `profile list`.
fn bound_inference(mut inference: Value) -> Value {
    if let Some(object) = inference.as_object_mut() {
        object.remove("profiles");
    }
    without_nulls(inference)
}

fn without_nulls(mut value: Value) -> Value {
    match &mut value {
        Value::Object(object) => {
            object.retain(|_, field| !field.is_null());
            for field in object.values_mut() {
                *field = without_nulls(field.take());
            }
        }
        Value::Array(items) => {
            for item in items {
                *item = without_nulls(item.take());
            }
        }
        _ => {}
    }
    value
}

/// An install receipt's empty `adopted`, `kept`, `removed` and `replaced`
/// lists say nothing; a present list is always non-empty.
fn without_empty_lists(mut receipt: Value) -> Value {
    if let Some(object) = receipt.as_object_mut() {
        object.retain(|_, field| field.as_array().is_none_or(|items| !items.is_empty()));
    }
    receipt
}

/// What a schema preview decides for an install: which collections it brings
/// and whether they need publishing. Contract digests stay with the schema tool.
fn schema_summary(schema: &impl serde::Serialize) -> anyhow::Result<Value> {
    let schema = serde_json::to_value(schema)?;
    let mut summary = json!({
        "collections": schema["collection_contracts"].as_object().map(|contracts| contracts.keys().collect::<Vec<_>>()),
        "requires_publication": schema["requires_publication"],
    });
    if schema["field_deltas"]
        .as_object()
        .is_some_and(|deltas| !deltas.is_empty())
    {
        summary["field_deltas"] = schema["field_deltas"].clone();
    }
    Ok(summary)
}
