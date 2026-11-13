use super::*;
use gents_session::session::output_render::ordered;

impl PackInstaller {
    pub async fn documents(
        &self,
        operation: &str,
        args: &PackChangeInput,
        distribution: &Distribution,
        commit: bool,
        edited: EditedDocuments,
    ) -> anyhow::Result<String> {
        let access = self.core.access()?;
        let packs = self.packs(&access);
        let node_binding = self.core.refresh_node_binding().await?;
        let plan = packs
            .plan_documents(operation, args, distribution, node_binding.as_ref())
            .await?;
        let (prior, dependencies, blockers) = (plan.prior, plan.dependencies, plan.blockers);
        if !commit && !plan.missing_slots.is_empty() {
            // Incomplete previews still must reject unauthorized supplied bindings.
            // Complete previews and installs retain the prepared-binding fence below.
            for preview in &plan.inspected {
                self.fence_bindings(&preview.bindings).await?;
            }
            return ordered! {
                "ready": false, "committed": false,
                "missing_inference_slots": plan.missing_slots,
                "next": "repeat --inference-slot NAME=PROFILE_ID for every missing slot, using exact eligible profile IDs",
                "inference": without_nulls(serde_json::to_value(&plan.inspected)?),
                "operation": operation,
                "package": distribution.manifest().name, "version": distribution.manifest().version,
                "installed": prior,
                "source": distribution.source(), "artifact_digest": distribution.digest(),
            }
            .pretty();
        }
        let provisioner = match (self.core.runtime_identity().ok(), self.actor.as_deref()) {
            (Some(runtime), Some(actor)) => Some(node_packs::AgentProvisioner {
                node: &self.node,
                runtime,
                actor: Some(actor),
                selected_agent_did: Some(self.core.agent_did()),
                node_binding: node_binding.as_ref(),
            }),
            _ => None,
        };
        let prepared = packs
            .prepare_documents(args, distribution, &dependencies, provisioner.as_ref())
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
        let schemas = packs.schema_previews(distribution).await?;
        let authority = packs.authority_asked(
            std::iter::once(distribution.manifest())
                .chain(dependencies.iter().map(|d| d.manifest())),
        )?;
        // A shipped job block is granted by this install under pack authority
        // (Lean `packJobBlockAdmissible`); the receipt records the grant and
        // what `check` leaves refused.
        let job_blocks = gents_documents::plugin_job_settings::dynamic_check_notices(
            prepared
                .config
                .tools
                .iter()
                .chain(prepared.dependencies.iter().flat_map(|d| &d.config.tools)),
        );
        let dependency_receipts = dependencies.iter().map(|dependency| json!({
            "coordinate": dependency.coordinate(),
            "version": dependency.manifest().version, "artifact_digest": dependency.digest(),
        })).collect::<Vec<_>>();
        if !commit {
            return ordered! {
                "ready": blockers.is_empty(), "committed": false,
                "apply_with": blockers.is_empty().then(|| json!({
                    "argv_prefix": ["pack", operation, distribution.pinned_spec(&args.package), "--digest", distribution.digest()],
                    "repeat_inference_slots": args.inference_slots, "repeat_variables": repeat_variables(&args.variables),
                })),
                "installation_blockers": blockers,
                "edited_documents": self.edited_documents(&distribution.coordinate(), false).await?,
                "would_grant": authority,
                "job_blocks": (!job_blocks.is_empty()).then_some(&job_blocks),
                "operation": operation,
                "package": distribution.manifest().name, "version": distribution.manifest().version,
                "installed": prior,
                "inference": bound_inference(serde_json::to_value(&prepared.inference)?),
                "dependency_inference": (!prepared.dependency_inference.is_empty()).then_some(&prepared.dependency_inference),
                "dependencies": (!dependency_receipts.is_empty()).then_some(&dependency_receipts),
                "schemas": schemas.iter().map(schema_summary).collect::<anyhow::Result<Vec<_>>>()?,
                "materialized_ids": NodePacks::materialized_ids(&prepared)?,
                "source": distribution.source(), "artifact_digest": distribution.digest(),
                "registry_artifact_digest": distribution.registry_artifact_digest(),
            }
            .pretty();
        }
        anyhow::ensure!(
            blockers.is_empty(),
            "pack plugins require operator authorization: {blockers:?}"
        );
        let (receipt, effective) = packs
            .install_documents(
                distribution,
                &dependencies,
                &prepared,
                gents_pack::operations::pack::drift_policy(edited),
                &gents_inference::resolved::ProviderProfilePolicy,
            )
            .await?;
        // Reader on a published root is the User's to grant, so an agent the
        // pack ships with host tools waits for it; the receipt names the step.
        // The pack and each dependency it installed: a dependency's agents
        // run under the same root as the pack's own.
        let coordinates: Vec<String> = std::iter::once(distribution.coordinate())
            .chain(
                dependencies
                    .iter()
                    .map(|dependency| dependency.coordinate()),
            )
            .collect();
        let host_root_access = crate::surface::pack_root_readers(
            &access,
            self.core.node_did(),
            &coordinates,
            PackActor::Agent,
        )
        .await?
        .into_iter()
        .map(|reader| {
            let published = (reader.action
                == gents_protocol::operations::pack::ReaderAction::ReportGrantStep)
                .then_some(reader.root.as_str());
            crate::surface::reader_grant_step(&reader.agent, published, prior.is_none())
        })
        .collect::<Vec<_>>();
        ordered! {
            "effect": "The document pack is installed.",
            "host_root_access": (!host_root_access.is_empty()).then_some(&host_root_access),
            "effective": effective,
            "install": without_empty_lists(serde_json::to_value(&receipt)?),
            "granted_authority": authority,
            "job_blocks": (!job_blocks.is_empty()).then_some(&job_blocks),
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

/// The supplied variables as the `--var` values that repeat them.
fn repeat_variables(variables: &gents_protocol::operations::pack::PackVariables) -> Vec<String> {
    let shared = variables
        .shared
        .iter()
        .map(|(name, value)| format!("{name}={value}"));
    let scoped = variables.scoped.iter().flat_map(|(coordinate, values)| {
        values
            .iter()
            .map(move |(name, value)| format!("{coordinate}:{name}={value}"))
    });
    shared.chain(scoped).collect()
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
