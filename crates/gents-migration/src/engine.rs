use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};
use std::sync::Arc;
use std::time::Instant;

use defra_node::{CollectionVersion, EmbeddedNode};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::error::{Error, Result};
use crate::lens;
use crate::materialize;
use crate::registry::{
    BaselineCollection, DynamicRegistry, MigrationStep, Registry, DEFAULT_REGISTRY,
};
use crate::report::MigrationReport;

/// Process-wide lock: desktop and runtime paths can race within one process.
static ENSURE_LOCK: Mutex<()> = Mutex::const_new(());

pub async fn ensure_migrations(node: &EmbeddedNode) -> Result<MigrationReport> {
    let mut baseline = DEFAULT_REGISTRY.baseline.to_vec();
    let mut steps = DEFAULT_REGISTRY.steps.to_vec();
    crate::upgrades::include_existing(node, &mut baseline, &mut steps)?;
    let registry = Registry {
        baseline: &baseline,
        steps: &steps,
        ordinary_indexes: DEFAULT_REGISTRY.ordinary_indexes,
    };
    ensure_migrations_with_registry(node, &registry).await
}

pub async fn ensure_migrations_with_registry(
    node: &EmbeddedNode,
    registry: &Registry<'_>,
) -> Result<MigrationReport> {
    let _guard = ENSURE_LOCK.lock().await;
    let mut report = MigrationReport::default();

    let started = Instant::now();
    reject_unknown_lineage(node, registry).await?;
    let file_image_owner = register_file_image_policy(node, registry).await?;
    register_baseline(node, registry, &mut report).await?;
    if let Some(owner) = file_image_owner.filter(|_| manages_file_images(registry)) {
        register_file_image_owner(node, owner, registry).await?;
    }
    let baseline_ready = Instant::now();
    apply_steps(node, registry, &mut report).await?;
    let steps_ready = Instant::now();
    verify_managed_lineages(node, registry, &mut report).await?;
    let lineages_ready = Instant::now();

    // Keep rescanning every collection with a versioned step: old-version
    // documents can arrive over P2P after a migration was first applied.
    // Unrelated collections need no pass because their baseline schema and
    // indexes are already current on writes and merges.
    let versioned_collections = versioned_step_collections(registry);
    if !versioned_collections.is_empty() {
        report.materialization =
            materialize::materialize_collections(node, &versioned_collections).await?;
    }
    let materialization_ready = Instant::now();
    reconcile_ordinary_indexes(node, registry).await?;
    let indexes_ready = Instant::now();
    report.warnings.extend(
        report
            .materialization
            .parked_unique_conflicts
            .iter()
            .cloned(),
    );

    info!(
        baseline_registered = report.baseline_registered,
        baseline_already_present = report.baseline_already_present,
        steps_applied = report.steps_applied,
        steps_already_current = report.steps_already_current,
        edges_repaired = report.edges_repaired,
        documents_materialized = report.materialization.documents_materialized,
        baseline_ms = baseline_ready.duration_since(started).as_millis(),
        steps_ms = steps_ready.duration_since(baseline_ready).as_millis(),
        lineage_verification_ms = lineages_ready.duration_since(steps_ready).as_millis(),
        materialization_ms = materialization_ready
            .duration_since(lineages_ready)
            .as_millis(),
        index_reconciliation_ms = indexes_ready
            .duration_since(materialization_ready)
            .as_millis(),
        elapsed_ms = indexes_ready.duration_since(started).as_millis(),
        "ensure_migrations complete"
    );

    Ok(report)
}

const FILE_COLLECTION_POLICIES: &[(&str, bool)] = &[
    (gents_protocol::schemas::TOOL_FILE_CHANGE_NAME, true),
    (gents_protocol::schemas::TOOL_FILE_INVENTORY_NAME, true),
    (gents_protocol::schemas::FILE_INVENTORY_REPLY_NAME, false),
    (gents_protocol::schemas::FILE_INVENTORY_PROGRESS_NAME, false),
    (gents_protocol::schemas::FILE_INVENTORY_PAGE_NAME, true),
];

fn manages_file_images(registry: &Registry<'_>) -> bool {
    registry.managed_names().any(|name| {
        FILE_COLLECTION_POLICIES
            .iter()
            .any(|(collection, _)| *collection == name)
    })
}

async fn register_file_image_policy<'n>(
    node: &'n EmbeddedNode,
    registry: &Registry<'_>,
) -> Result<Option<&'n str>> {
    if !manages_file_images(registry)
        && !registry
            .managed_names()
            .any(|name| name == gents_protocol::schemas::FILE_IMAGE_HYDRATION_RECEIPT_NAME)
    {
        return Ok(None);
    }
    for (name, branchable) in FILE_COLLECTION_POLICIES {
        if let Some(collection) = node
            .get_collection(name)
            .map_err(|error| Error::Node(error.into()))?
        {
            validate_file_image_policy(&collection, *branchable)?;
        }
    }
    if let Some(collection) = node
        .get_collection(gents_protocol::schemas::FILE_IMAGE_HYDRATION_RECEIPT_NAME)
        .map_err(|error| Error::Node(error.into()))?
    {
        validate_file_image_policy(&collection, false)?;
    }
    let owner = node.node_identity_did().ok_or_else(|| {
        Error::Node(anyhow::anyhow!(
            "retained file image storage requires the configured node identity"
        ))
    })?;
    let policy = node
        .add_dac_policy_with_counter(owner, gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY, 0)
        .await
        .map_err(|error| Error::Node(error.into()))?;
    if policy != gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY_ID {
        return Err(Error::Node(anyhow::anyhow!(
            "retained file image policy does not match its schema binding"
        )));
    }
    Ok(Some(owner))
}

/// Policy bindings are outside the pinned schema version; lineage validation
/// alone cannot establish the retained images' document permissions.
fn validate_file_image_policy(collection: &CollectionVersion, branchable: bool) -> Result<()> {
    if collection.is_branchable != branchable
        || !collection.policy.as_ref().is_some_and(|policy| {
            policy.id == gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY_ID
                && policy.resource_name == gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY_RESOURCE
        })
    {
        return Err(Error::Node(anyhow::anyhow!(
            "retained file image collection has an incompatible document policy"
        )));
    }
    Ok(())
}

/// DefraDB's branchable read guard falls back to the collection object when a
/// document has no local ACP registration. That fallback must remain private.
async fn register_file_image_owner(
    node: &EmbeddedNode,
    owner: &str,
    registry: &Registry<'_>,
) -> Result<()> {
    let owner = owner
        .parse()
        .map_err(|error| Error::Node(anyhow::anyhow!("invalid image owner DID: {error}")))?;
    for (name, branchable) in FILE_COLLECTION_POLICIES {
        if !registry.managed_names().any(|managed| managed == *name) {
            continue;
        }
        let collection = node
            .get_collection(name)
            .map_err(|error| Error::Node(error.into()))?
            .ok_or_else(|| Error::CollectionMissing {
                collection: (*name).into(),
            })?;
        validate_file_image_policy(&collection, *branchable)?;
        let acp = node.document_acp();
        let policy = gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY_ID;
        let resource = gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY_RESOURCE;
        if acp
            .is_doc_registered(policy, resource, &collection.collection_id)
            .await
            .map_err(|error| Error::Node(error.into()))?
        {
            let existing_owner = acp
                .get_doc_owner(policy, resource, &collection.collection_id)
                .await
                .map_err(|error| Error::Node(error.into()))?;
            if existing_owner.as_ref() != Some(&owner) {
                return Err(Error::Node(anyhow::anyhow!(
                    "retained file image collection belongs to a different node"
                )));
            }
        } else {
            acp.register_object(&owner, policy, resource, &collection.collection_id)
                .await
                .map_err(|error| Error::Node(error.into()))?;
        }
    }
    Ok(())
}

fn versioned_step_collections<'a>(registry: &Registry<'a>) -> Vec<&'a str> {
    let mut seen = RapidHashSet::new();
    registry
        .steps
        .iter()
        .filter_map(|step| match step {
            MigrationStep::PatchVersioned { collection, .. } => Some(*collection),
            MigrationStep::AddCollection { .. } | MigrationStep::PatchInPlace { .. } => None,
        })
        .filter(|collection| seen.insert(*collection))
        .collect()
}

pub async fn ensure_migrations_arc(node: Arc<EmbeddedNode>) -> Result<MigrationReport> {
    ensure_migrations(node.as_ref()).await
}

pub async fn ensure_migrations_dynamic(
    node: &EmbeddedNode,
    registry: &DynamicRegistry,
) -> Result<MigrationReport> {
    let (baseline, steps) = registry.as_registry();
    let view = Registry {
        baseline: &baseline,
        steps: &steps,
        ordinary_indexes: &[],
    };
    ensure_migrations_with_registry(node, &view).await
}

async fn register_baseline(
    node: &EmbeddedNode,
    registry: &Registry<'_>,
    report: &mut MigrationReport,
) -> Result<()> {
    let mut missing = Vec::new();
    for entry in registry.baseline {
        if node
            .get_collection(entry.name)
            .map_err(|error| Error::Node(error.into()))?
            .is_some()
        {
            report.baseline_already_present += 1;
            debug!(collection = entry.name, "baseline schema already present");
        } else {
            missing.push(entry);
        }
    }
    if missing.is_empty() {
        return Ok(());
    }

    let batch = missing
        .iter()
        .map(|entry| entry.sdl)
        .collect::<Vec<_>>()
        .join("\n");
    node.add_schema(&batch)
        .await
        .map_err(|source| Error::BaselineRegister {
            collection: missing
                .iter()
                .map(|entry| entry.name)
                .collect::<Vec<_>>()
                .join(","),
            source: source.into(),
        })?;
    report.baseline_registered += missing.len();
    debug!(
        collections = missing.len(),
        "baseline schemas registered in one batch"
    );
    Ok(())
}

async fn reconcile_ordinary_indexes(node: &EmbeddedNode, registry: &Registry<'_>) -> Result<()> {
    for index in registry.ordinary_indexes {
        let fields = index
            .fields
            .iter()
            .map(|field| schema::IndexedFieldDescription {
                name: field.name.to_owned(),
                descending: field.descending,
            })
            .collect();
        node.ensure_ordinary_index(index.collection, index.name, fields)
            .await
            .map_err(|error| Error::Node(error.into()))?;
    }
    Ok(())
}

async fn apply_steps(
    node: &EmbeddedNode,
    registry: &Registry<'_>,
    report: &mut MigrationReport,
) -> Result<()> {
    for step in registry.steps {
        match step {
            MigrationStep::AddCollection {
                id,
                sdl,
                expected_version,
                expected_state,
            } => {
                apply_add_collection(node, id, sdl, *expected_version, expected_state, report)
                    .await?;
            }
            MigrationStep::PatchVersioned {
                id,
                collection,
                patch,
                lens: lens_spec,
                expected_version,
                expected_transform,
                expected_state,
            } => {
                apply_patch_versioned(
                    node,
                    StepContext {
                        id,
                        collection,
                        lens_spec: *lens_spec,
                        expected_state,
                    },
                    patch,
                    *expected_version,
                    *expected_transform,
                    report,
                )
                .await?;
            }
            MigrationStep::PatchInPlace {
                id,
                collection,
                patch,
                expected_state,
            } => {
                apply_patch_in_place(node, id, collection, patch, expected_state, report).await?;
            }
        }
    }
    Ok(())
}

async fn apply_add_collection(
    node: &EmbeddedNode,
    id: &str,
    sdl: &str,
    expected_version: Option<&str>,
    expected_state: &crate::expectation::CollectionExpectation,
    report: &mut MigrationReport,
) -> Result<()> {
    match node.add_schema(sdl).await {
        Ok(()) => report.steps_applied += 1,
        Err(error) if error.to_string().contains("already exists") => {
            report.steps_already_current += 1;
        }
        Err(error) => {
            return Err(Error::StepFailed {
                step: id.to_string(),
                collection: id.to_string(),
                source: error.into(),
            });
        }
    }

    if let Some(pin) = expected_version {
        let versions = node
            .get_all_collection_versions()
            .await
            .map_err(|error| Error::Node(error.into()))?;
        let Some(active) = versions
            .iter()
            .find(|v| v.version_id == pin && !v.is_placeholder)
        else {
            return Err(Error::VersionPinMismatch {
                collection: id.to_string(),
                expected: pin.to_string(),
                actual: "<missing>".into(),
            });
        };
        expected_state
            .verify(active)
            .map_err(|detail| Error::StateVerification {
                collection: id.to_string(),
                step: id.to_string(),
                detail,
            })?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct StepContext<'a> {
    id: &'a str,
    collection: &'a str,
    lens_spec: Option<crate::registry::LensSpec<'a>>,
    expected_state: &'a crate::expectation::CollectionExpectation,
}

async fn apply_patch_versioned(
    node: &EmbeddedNode,
    ctx: StepContext<'_>,
    patch: &str,
    expected_version: Option<&str>,
    expected_transform: Option<&str>,
    report: &mut MigrationReport,
) -> Result<()> {
    let StepContext {
        id,
        collection,
        lens_spec,
        expected_state,
    } = ctx;
    let Some(pin) = expected_version else {
        return Err(Error::StepFailed {
            step: id.to_string(),
            collection: collection.to_string(),
            source: anyhow::anyhow!("PatchVersioned steps require expected_version pin"),
        });
    };

    let active = node
        .get_collection(collection)
        .map_err(|error| Error::Node(error.into()))?
        .ok_or_else(|| Error::CollectionMissing {
            collection: collection.to_string(),
        })?;

    if active.version_id == pin && active.is_active && !active.is_placeholder {
        verify_active_step(node, ctx, pin, expected_transform, report).await?;
        report.steps_already_current += 1;
        return Ok(());
    }

    let all = node
        .get_all_collection_versions()
        .await
        .map_err(|error| Error::Node(error.into()))?;
    let dest = all.iter().find(|v| v.version_id == pin);

    // A later step in the same collection may already be active. In that
    // state this destination is an inactive ancestor, not a crash-window
    // version waiting to be reactivated. Re-activating it would roll the
    // collection backward on every ensure before the later step rolls it
    // forward again.
    if let Some(dest) = dest {
        if !dest.is_active && version_descends_from(&all, &active.version_id, pin) {
            expected_state
                .verify(dest)
                .map_err(|detail| Error::StateVerification {
                    collection: collection.to_string(),
                    step: id.to_string(),
                    detail,
                })?;
            if let Some(expected_tx) = expected_transform {
                let source = dest
                    .previous_version
                    .as_ref()
                    .map(|previous| previous.source_collection_id.as_str())
                    .ok_or_else(|| Error::StateVerification {
                        collection: collection.to_string(),
                        step: id.to_string(),
                        detail: "versioned migration destination is missing its source edge"
                            .to_string(),
                    })?;
                repair_transform_if_needed(node, ctx, source, pin, expected_tx, dest, report)
                    .await?;
            }
            report.steps_already_current += 1;
            return Ok(());
        }
    }

    match dest {
        None => {
            if let Some(spec) = lens_spec {
                let cfg = lens::lens_config(&spec, &active.version_id, pin);
                node.set_migration(cfg)
                    .await
                    .map_err(|e| Error::StepFailed {
                        step: id.to_string(),
                        collection: collection.to_string(),
                        source: e.into(),
                    })?;
            }
            let patched = node
                .patch_collection(collection, patch)
                .await
                .map_err(|e| Error::StepFailed {
                    step: id.to_string(),
                    collection: collection.to_string(),
                    source: e.into(),
                })?;
            if patched.version_id != pin {
                return Err(Error::VersionPinMismatch {
                    collection: collection.to_string(),
                    expected: pin.to_string(),
                    actual: patched.version_id,
                });
            }
            expected_state
                .verify(&patched)
                .map_err(|detail| Error::StateVerification {
                    collection: collection.to_string(),
                    step: id.to_string(),
                    detail,
                })?;
            node.set_active_collection_version(pin)
                .await
                .map_err(|e| Error::StepFailed {
                    step: id.to_string(),
                    collection: collection.to_string(),
                    source: e.into(),
                })?;
            report.steps_applied += 1;
        }
        Some(v) if v.is_placeholder => {
            let patched = node
                .patch_collection(collection, patch)
                .await
                .map_err(|e| Error::StepFailed {
                    step: id.to_string(),
                    collection: collection.to_string(),
                    source: e.into(),
                })?;
            if patched.version_id != pin {
                return Err(Error::VersionPinMismatch {
                    collection: collection.to_string(),
                    expected: pin.to_string(),
                    actual: patched.version_id,
                });
            }
            expected_state
                .verify(&patched)
                .map_err(|detail| Error::StateVerification {
                    collection: collection.to_string(),
                    step: id.to_string(),
                    detail,
                })?;
            node.set_active_collection_version(pin)
                .await
                .map_err(|e| Error::StepFailed {
                    step: id.to_string(),
                    collection: collection.to_string(),
                    source: e.into(),
                })?;
            report.steps_applied += 1;
        }
        Some(v) if !v.is_active => {
            expected_state
                .verify(v)
                .map_err(|detail| Error::StateVerification {
                    collection: collection.to_string(),
                    step: id.to_string(),
                    detail,
                })?;
            if let Some(expected_tx) = expected_transform {
                repair_transform_if_needed(
                    node,
                    ctx,
                    &active.version_id,
                    pin,
                    expected_tx,
                    v,
                    report,
                )
                .await?;
            }
            node.set_active_collection_version(pin)
                .await
                .map_err(|e| Error::StepFailed {
                    step: id.to_string(),
                    collection: collection.to_string(),
                    source: e.into(),
                })?;
            report.steps_applied += 1;
        }
        Some(v) => {
            verify_active_step(node, ctx, pin, expected_transform, report).await?;
            if v.version_id == pin {
                if let Err(e) = node.set_active_collection_version(pin).await {
                    warn!(
                        step = id,
                        collection,
                        error = %e,
                        "post-activation reindex repair failed"
                    );
                    report.warnings.push(format!(
                        "post-activation repair failed for {collection} step {id}: {e}"
                    ));
                }
            }
            report.steps_already_current += 1;
        }
    }

    Ok(())
}

fn version_descends_from(
    versions: &[CollectionVersion],
    descendant_version: &str,
    ancestor_version: &str,
) -> bool {
    let mut current = descendant_version;
    let mut visited = RapidHashSet::new();
    while visited.insert(current) {
        let Some(version) = versions
            .iter()
            .find(|version| version.version_id == current)
        else {
            return false;
        };
        let Some(previous) = version.previous_version.as_ref() else {
            return false;
        };
        if previous.source_collection_id == ancestor_version {
            return true;
        }
        current = &previous.source_collection_id;
    }
    false
}

async fn verify_active_step(
    node: &EmbeddedNode,
    ctx: StepContext<'_>,
    pin: &str,
    expected_transform: Option<&str>,
    report: &mut MigrationReport,
) -> Result<()> {
    let StepContext {
        id,
        collection,
        expected_state,
        ..
    } = ctx;
    let active = node
        .get_collection(collection)
        .map_err(|error| Error::Node(error.into()))?
        .ok_or_else(|| Error::CollectionMissing {
            collection: collection.to_string(),
        })?;
    if active.version_id != pin {
        return Err(Error::VersionPinMismatch {
            collection: collection.to_string(),
            expected: pin.to_string(),
            actual: active.version_id,
        });
    }
    expected_state
        .verify(&active)
        .map_err(|detail| Error::StateVerification {
            collection: collection.to_string(),
            step: id.to_string(),
            detail,
        })?;
    if let Some(expected_tx) = expected_transform {
        let prev = active.version_id.clone();
        if let Some(ref pv) = active.previous_version {
            let source = pv.source_collection_id.clone();
            repair_transform_if_needed(node, ctx, &source, pin, expected_tx, &active, report)
                .await?;
            let _ = prev;
        }
    }
    Ok(())
}

async fn repair_transform_if_needed(
    node: &EmbeddedNode,
    ctx: StepContext<'_>,
    source: &str,
    dest: &str,
    expected_tx: &str,
    version: &CollectionVersion,
    report: &mut MigrationReport,
) -> Result<()> {
    let StepContext {
        id,
        collection,
        lens_spec,
        ..
    } = ctx;
    let current = version
        .previous_version
        .as_ref()
        .and_then(|pv| pv.transform.as_deref());
    if current == Some(expected_tx) {
        return Ok(());
    }
    let Some(spec) = lens_spec else {
        return Err(Error::StateVerification {
            collection: collection.to_string(),
            step: id.to_string(),
            detail: format!(
                "transform missing or mismatched (have {current:?}, want {expected_tx}) and no lens bytes to repair"
            ),
        });
    };
    let cfg = lens::lens_config(&spec, source, dest);
    node.set_migration(cfg)
        .await
        .map_err(|e| Error::StepFailed {
            step: id.to_string(),
            collection: collection.to_string(),
            source: e.into(),
        })?;
    report.edges_repaired += 1;
    info!(
        step = id,
        collection, source, dest, "repaired missing migration edge transform"
    );
    Ok(())
}

async fn apply_patch_in_place(
    node: &EmbeddedNode,
    id: &str,
    collection: &str,
    patch: &str,
    expected_state: &crate::expectation::CollectionExpectation,
    report: &mut MigrationReport,
) -> Result<()> {
    let active = node
        .get_collection(collection)
        .map_err(|error| Error::Node(error.into()))?
        .ok_or_else(|| Error::CollectionMissing {
            collection: collection.to_string(),
        })?;

    if expected_state.verify(&active).is_ok() {
        report.steps_already_current += 1;
        return Ok(());
    }

    let patched = node
        .patch_collection(collection, patch)
        .await
        .map_err(|e| Error::StepFailed {
            step: id.to_string(),
            collection: collection.to_string(),
            source: e.into(),
        })?;
    expected_state
        .verify(&patched)
        .map_err(|detail| Error::StateVerification {
            collection: collection.to_string(),
            step: id.to_string(),
            detail,
        })?;
    report.steps_applied += 1;
    Ok(())
}

fn versions_by_name(
    all_versions: &[CollectionVersion],
) -> RapidHashMap<String, Vec<&CollectionVersion>> {
    let mut by_name: RapidHashMap<String, Vec<&CollectionVersion>> = RapidHashMap::new();
    for v in all_versions {
        if v.is_placeholder || v.name.is_empty() {
            continue;
        }
        by_name.entry(v.name.clone()).or_default().push(v);
    }
    by_name
}

/// Reject a store whose existing managed collections descend from a lineage
/// this registry does not know, before anything is registered or patched.
async fn reject_unknown_lineage(node: &EmbeddedNode, registry: &Registry<'_>) -> Result<()> {
    let all_versions = node
        .get_all_collection_versions()
        .await
        .map_err(|error| Error::Node(error.into()))?;
    let by_name = versions_by_name(&all_versions);
    let known_pins = known_pin_set(registry);
    for entry in registry.baseline {
        if let Some(versions) = by_name.get(entry.name) {
            check_known_lineage(entry, versions, &known_pins)?;
        }
    }
    Ok(())
}

async fn verify_managed_lineages(
    node: &EmbeddedNode,
    registry: &Registry<'_>,
    _report: &mut MigrationReport,
) -> Result<()> {
    let all_versions = node
        .get_all_collection_versions()
        .await
        .map_err(|error| Error::Node(error.into()))?;
    let by_name = versions_by_name(&all_versions);

    let known_pins = known_pin_set(registry);
    let target_active = target_active_pins(registry);

    for entry in registry.baseline {
        verify_one_collection(entry, &by_name, &known_pins, &target_active)?;
    }

    for step in registry.steps {
        if let MigrationStep::AddCollection {
            expected_version: Some(pin),
            expected_state,
            ..
        } = step
        {
            let Some(v) = all_versions.iter().find(|v| v.version_id == *pin) else {
                return Err(Error::VersionPinMismatch {
                    collection: step.id().to_string(),
                    expected: pin.to_string(),
                    actual: "<missing>".into(),
                });
            };
            expected_state
                .verify(v)
                .map_err(|detail| Error::StateVerification {
                    collection: v.name.clone(),
                    step: step.id().to_string(),
                    detail,
                })?;
        }
    }

    Ok(())
}

fn known_pin_set(registry: &Registry<'_>) -> RapidHashSet<String> {
    let mut pins = RapidHashSet::new();
    for b in registry.baseline {
        if let Some(v) = b.expected_version {
            pins.insert(v.to_string());
        }
    }
    for step in registry.steps {
        match step {
            MigrationStep::AddCollection {
                expected_version: Some(v),
                ..
            }
            | MigrationStep::PatchVersioned {
                expected_version: Some(v),
                ..
            } => {
                pins.insert((*v).to_string());
            }
            _ => {}
        }
    }
    pins
}

fn target_active_pins(registry: &Registry<'_>) -> RapidHashMap<String, String> {
    let mut map = RapidHashMap::new();
    for b in registry.baseline {
        if let Some(v) = b.expected_version {
            map.insert(b.name.to_string(), v.to_string());
        }
    }
    for step in registry.steps {
        if let MigrationStep::PatchVersioned {
            collection,
            expected_version: Some(v),
            ..
        } = step
        {
            map.insert(collection.to_string(), (*v).to_string());
        }
    }
    map
}

fn verify_one_collection(
    entry: &BaselineCollection<'_>,
    by_name: &RapidHashMap<String, Vec<&CollectionVersion>>,
    known_pins: &RapidHashSet<String>,
    target_active: &RapidHashMap<String, String>,
) -> Result<()> {
    let versions = by_name
        .get(entry.name)
        .ok_or_else(|| Error::CollectionMissing {
            collection: entry.name.to_string(),
        })?;

    let non_ph: Vec<&CollectionVersion> = versions
        .iter()
        .copied()
        .filter(|v| !v.is_placeholder)
        .collect();

    if non_ph.is_empty() {
        return Err(Error::CollectionMissing {
            collection: entry.name.to_string(),
        });
    }

    check_known_lineage(entry, &non_ph, known_pins)?;

    let active = non_ph
        .iter()
        .find(|v| v.is_active)
        .or_else(|| non_ph.first())
        .copied()
        .ok_or_else(|| Error::CollectionMissing {
            collection: entry.name.to_string(),
        })?;

    if let Some(target) = target_active.get(entry.name) {
        if active.version_id != *target {
            return Err(Error::VersionPinMismatch {
                collection: entry.name.to_string(),
                expected: target.clone(),
                actual: active.version_id.clone(),
            });
        }
    }

    entry
        .expected_state
        .verify(active)
        .map_err(|detail| Error::StateVerification {
            collection: entry.name.to_string(),
            step: "baseline".into(),
            detail,
        })?;

    Ok(())
}

/// A collection's DAG belongs to this registry's lineage: the baseline root
/// pin is present and, when it has several versions, every one is a known pin.
fn check_known_lineage(
    entry: &BaselineCollection<'_>,
    versions: &[&CollectionVersion],
    known_pins: &RapidHashSet<String>,
) -> Result<()> {
    let non_ph: Vec<&CollectionVersion> = versions
        .iter()
        .copied()
        .filter(|v| !v.is_placeholder)
        .collect();
    if let Some(root) = entry.expected_version {
        if !non_ph.iter().any(|v| v.version_id == root) {
            return Err(Error::UnknownLineage {
                collection: entry.name.to_string(),
                versions: non_ph
                    .iter()
                    .map(|v| v.version_id.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            });
        }
    }
    if non_ph.len() > 1 {
        if let Some(foreign) = non_ph
            .iter()
            .find(|v| !known_pins.contains(v.version_id.as_str()))
        {
            if known_pins.is_empty() {
                return Err(Error::UnknownLineage {
                    collection: entry.name.to_string(),
                    versions: non_ph
                        .iter()
                        .map(|v| v.version_id.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                });
            }
            return Err(Error::ForeignVersion {
                collection: entry.name.to_string(),
                version_id: foreign.version_id.clone(),
            });
        }
    }
    Ok(())
}
