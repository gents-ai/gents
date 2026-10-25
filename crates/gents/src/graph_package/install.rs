use super::catalog::GraphPackageError;
use super::LoadedGraphPackage;
use crate::config_client::{
    apply_desired_state_plan, collection_schema_contract_digest, ConfigAccess, ConfigClientError,
    DesiredStateApplyPlan, DesiredStateError, TransactionFailure,
};
use crate::graph_pipeline::{
    bind_package_plan, compile_graph, BundledProvenance, CompilerPolicy, GraphIntent,
    GraphPipelineError, GraphPlan, PackagePlan, PlannedPackageArtifact, RequiredSchemaDigest,
};
use crate::Collection;
use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

type Result<T, E = GraphPackageInstallError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum GraphPackageInstallError {
    #[error(transparent)]
    ConfigClient(#[from] ConfigClientError),
    #[error(transparent)]
    DesiredState(#[from] DesiredStateError),
    #[error(transparent)]
    GraphPipeline(#[from] GraphPipelineError),
    #[error(transparent)]
    GraphPackage(#[from] GraphPackageError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Installation(Box<crate::pack::InstallationError>),
    #[error(transparent)]
    Provenance(Box<crate::pack::ProvenanceError>),
    #[error("package owner DID must not be blank")]
    OwnerDidBlank,
    #[error("package owner node is missing")]
    OwnerNodeMissing,
    #[error("package owner node is disabled")]
    OwnerNodeDisabled,
    #[error("owned graph definition missing graph_id")]
    GraphDefinitionMissingId,
    #[error("multiple installed graphs belong to package {package_name:?}")]
    MultipleInstalledGraphs { package_name: String },
    #[error("active revision missing plan_json")]
    ActiveRevisionMissingPlan,
    #[error("active package revision is missing or ambiguous")]
    ActiveRevisionAmbiguous,
    #[error("active package revision failed immutable identity verification")]
    ActiveRevisionUnverified,
    #[error("active graph revision has no package attribution")]
    NoPackageAttribution,
    #[error(
        "the active revision of graph {graph_id} belongs to package {package:?}, not {name:?}; list the installed graphs again"
    )]
    RevisionPackageMismatch {
        graph_id: String,
        package: String,
        name: String,
    },
    #[error("{coordinate} has no installation record; install it again under that coordinate")]
    NoInstallationRecord { coordinate: String },
    #[error(
        "the active revision of {coordinate} was built from {built} but the installed pack is {installed}; install {coordinate} again"
    )]
    RevisionDigestMismatch {
        coordinate: String,
        built: String,
        installed: String,
    },
    #[error(
        "graph {graph_id} is active at revision {active}, not {digest}; pass the active digest to run it"
    )]
    PinnedRevisionMismatch {
        graph_id: String,
        active: String,
        digest: String,
    },
    #[error("graph owner differs from installation scope")]
    GraphOwnerMismatch,
    #[error("configuration logical ID missing")]
    ConfigLogicalIdMissing,
    #[error("package schema {path:?} declares no collection")]
    SchemaWithoutCollection { path: String },
    #[error("package schema repeats a collection")]
    SchemaRepeatsCollection,
    #[error(
        "entry {entry:?} prepare.writes names {collection:?}, which package {package:?} does not declare in its schemas"
    )]
    EntryWritesUndeclared {
        entry: String,
        collection: String,
        package: String,
    },
    #[error("identical package installation did not reproduce active revision")]
    IdenticalInstallDiverged,
    #[error("package install requires graph owner authority")]
    ActorNotOwner,
}

impl From<crate::pack::ProvenanceError> for GraphPackageInstallError {
    fn from(error: crate::pack::ProvenanceError) -> Self {
        Self::Provenance(Box::new(error))
    }
}

impl From<crate::pack::InstallationError> for GraphPackageInstallError {
    fn from(error: crate::pack::InstallationError) -> Self {
        Self::Installation(Box::new(error))
    }
}

impl TransactionFailure for GraphPackageInstallError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::ConfigClient(error) => Some(error),
            Self::DesiredState(error) => error.config_client_error(),
            Self::GraphPipeline(error) => error.config_client_error(),
            Self::Installation(error) => error.config_client_error(),
            Self::Provenance(error) => error.config_client_error(),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphPackageInstallBindings {
    pub node_did: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inference_slots: crate::pack::PackInferenceBindings,
}

#[derive(Clone, Debug, Default)]
pub struct GraphInstallRecord {
    pub plugins: Vec<crate::pack::InstalledPackPlugin>,
    pub explicit: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GraphPackageInstallReceipt {
    pub package_name: String,
    pub package_version: String,
    pub package_digest: String,
    pub graph_id: String,
    pub revision_digest: String,
    pub predecessor_revision_digest: Option<String>,
    pub artifacts_complete: bool,
    pub desired_documents: usize,
    pub schema_digests: Vec<RequiredSchemaDigest>,
}

pub struct PreparedGraphPackageInstall {
    pub plan: GraphPlan,
    pub desired_state: DesiredStateApplyPlan,
    pub schema_digests: Vec<RequiredSchemaDigest>,
    /// Observed outside any transaction; re-checked inside the commit
    /// transaction so a concurrent activation is refused (#1651).
    expected_active_revision_digest: Option<String>,
}

fn selected_intent<'a>(
    graph_intents: &'a [GraphIntent],
    graph_id: Option<&str>,
) -> Result<&'a GraphIntent, GraphPackageError> {
    if let Some(graph_id) = graph_id {
        let mut matches = graph_intents
            .iter()
            .filter(|intent| intent.graph_id == graph_id);
        let selected = matches
            .next()
            .ok_or(GraphPackageError::SelectedGraphNotInPackage)?;
        if matches.next().is_some() {
            return Err(GraphPackageError::SelectedGraphAmbiguous);
        }
        return Ok(selected);
    }
    match graph_intents {
        [intent] => Ok(intent),
        [] => Err(GraphPackageError::PackageNoGraphIntent),
        _ => Err(GraphPackageError::PackageMultipleGraphs),
    }
}

async fn validate_owner(access: &ConfigAccess, owner: &str) -> Result<()> {
    if owner.trim().is_empty() {
        return Err(GraphPackageInstallError::OwnerDidBlank);
    }
    access
        .transact_readonly("graph_package.owner", |txn| {
            Box::pin(async move {
                let principal = crate::config_client::read_desired_state_document_in_txn(
                    txn,
                    Collection::Node,
                    owner,
                    owner,
                )
                .await?
                .ok_or(GraphPackageInstallError::OwnerNodeMissing)?;
                if principal["enabled"].as_bool() != Some(true) {
                    return Err(GraphPackageInstallError::OwnerNodeDisabled);
                }
                Ok(())
            })
        })
        .await
}

pub async fn default_graph_package_install_bindings(
    access: &ConfigAccess,
    package: &LoadedGraphPackage,
    owner_did: &str,
    requested: &crate::pack::PackInferenceBindings,
) -> anyhow::Result<GraphPackageInstallBindings> {
    validate_owner(access, owner_did).await?;
    let preview = crate::pack::preview_pack_inference_bindings(
        access,
        &package.manifest,
        owner_did,
        requested,
    )
    .await?;
    Ok(GraphPackageInstallBindings {
        node_did: owner_did.to_owned(),
        inference_slots: preview.bindings,
    })
}

pub async fn load_installed_package_plan(
    access: &ConfigAccess,
    package_name: &str,
    owner_did: &str,
) -> Result<Option<GraphPlan>> {
    access
        .transact_readonly("graph_package.select_installed", move |txn| {
            Box::pin(async move {
                let response = txn
                    .execute(&format!(
                        "{{ GraphDefinition(filter: {{node_did: {{_eq: \"{}\"}}}}) {{graph_id}} }}",
                        crate::graphql::escape_graphql_string(owner_did),
                    ))
                    .await?;
                let mut selected = None;
                for definition in response_rows(&response, "GraphDefinition") {
                    let graph_id = definition["graph_id"]
                        .as_str()
                        .ok_or(GraphPackageInstallError::GraphDefinitionMissingId)?;
                    let Some(plan) = crate::graph_pipeline::load_active_graph_plan_in_txn(
                        txn, owner_did, graph_id,
                    )
                    .await?
                    else {
                        continue;
                    };
                    if plan
                        .package
                        .as_ref()
                        .is_some_and(|package| package.name == package_name)
                    {
                        if selected.is_some() {
                            return Err(GraphPackageInstallError::MultipleInstalledGraphs {
                                package_name: package_name.to_owned(),
                            });
                        }
                        selected = Some(plan);
                    }
                }
                Ok(selected)
            })
        })
        .await
}

#[derive(Clone, Copy, Debug)]
pub enum GraphRunSelector<'a> {
    Package {
        name: &'a str,
        coordinate: Option<&'a str>,
    },
    Pinned {
        graph_id: &'a str,
        digest: &'a str,
    },
}

pub async fn select_run_plan(
    access: &ConfigAccess,
    owner_did: &str,
    selector: &GraphRunSelector<'_>,
) -> Result<Option<GraphPlan>> {
    match *selector {
        GraphRunSelector::Package { name, coordinate } => {
            let Some(plan) = load_installed_package_plan(access, name, owner_did).await? else {
                return Ok(None);
            };
            let attribution = plan
                .package
                .as_ref()
                .ok_or(GraphPackageInstallError::NoPackageAttribution)?;
            if attribution.name != name {
                return Err(GraphPackageInstallError::RevisionPackageMismatch {
                    graph_id: plan.graph_id.clone(),
                    package: attribution.name.clone(),
                    name: name.to_owned(),
                });
            }
            if let Some(coordinate) = coordinate {
                let record = crate::pack::read_installed_pack(access, owner_did, coordinate)
                    .await?
                    .ok_or_else(|| GraphPackageInstallError::NoInstallationRecord {
                        coordinate: coordinate.to_owned(),
                    })?;
                if attribution.package_digest != record.digest {
                    return Err(GraphPackageInstallError::RevisionDigestMismatch {
                        coordinate: coordinate.to_owned(),
                        built: attribution.package_digest.clone(),
                        installed: record.digest.clone(),
                    });
                }
            }
            Ok(Some(plan))
        }
        GraphRunSelector::Pinned { graph_id, digest } => {
            let Some(plan) = crate::graph_pipeline::load_active_graph_plan_with_access(
                access, owner_did, graph_id,
            )
            .await?
            else {
                return Ok(None);
            };
            if plan.digest != digest {
                return Err(GraphPackageInstallError::PinnedRevisionMismatch {
                    graph_id: graph_id.to_owned(),
                    active: plan.digest.clone(),
                    digest: digest.to_owned(),
                });
            }
            Ok(Some(plan))
        }
    }
}

fn response_rows<'a>(response: &'a Value, collection: &str) -> &'a [Value] {
    response
        .get("data")
        .and_then(|data| data.get(collection))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}
fn digest_bytes(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn active_revision_digest_query(owner: &str, graph_id: &str) -> String {
    format!(
        r#"{{ GraphDefinition(filter: {{ node_did: {{ _eq: "{}" }}, graph_id: {{ _eq: "{}" }} }}, limit: 2) {{ active_revision_digest }} }}"#,
        crate::graphql::escape_graphql_string(owner),
        crate::graphql::escape_graphql_string(graph_id)
    )
}

fn active_revision_digest(response: &Value) -> Result<Option<String>, GraphPackageError> {
    let definitions = response_rows(response, "GraphDefinition");
    if definitions.len() > 1 {
        return Err(GraphPackageError::PackageGraphDefinitionAmbiguous);
    }
    Ok(definitions
        .first()
        .and_then(|row| row["active_revision_digest"].as_str())
        .filter(|digest| !digest.is_empty())
        .map(str::to_owned))
}

async fn active_revision_plan(
    access: &ConfigAccess,
    owner: &str,
    graph_id: &str,
) -> Result<Option<GraphPlan>> {
    let escaped_owner = crate::graphql::escape_graphql_string(owner);
    let response = access
        .execute(&active_revision_digest_query(owner, graph_id))
        .await?;
    let Some(digest) = active_revision_digest(&response)? else {
        return Ok(None);
    };
    let digest = digest.as_str();
    let response = access.execute(&format!(r#"{{ GraphRevision(filter: {{ owner_did: {{ _eq: "{escaped_owner}" }}, graph_id: {{ _eq: "{}" }}, digest: {{ _eq: "{}" }} }}, limit: 2) {{ plan_json }} }}"#,
        crate::graphql::escape_graphql_string(graph_id), crate::graphql::escape_graphql_string(digest))).await?;
    let revisions = response_rows(&response, "GraphRevision");
    if revisions.len() != 1 {
        return Err(GraphPackageInstallError::ActiveRevisionAmbiguous);
    }
    let plan: GraphPlan = serde_json::from_str(
        revisions[0]["plan_json"]
            .as_str()
            .ok_or(GraphPackageInstallError::ActiveRevisionMissingPlan)?,
    )?;
    if !(plan.graph_id == graph_id
        && plan.digest == digest
        && crate::graph_pipeline::verify_graph_plan_digest(&plan))
    {
        return Err(GraphPackageInstallError::ActiveRevisionUnverified);
    }
    Ok(Some(plan))
}

fn same_install_configuration(active: &GraphPlan, base: &GraphPlan, package: &PackagePlan) -> bool {
    let mut active = active.clone();
    active.digest.clear();
    let Some(active_package) = active.package.as_mut() else {
        return false;
    };
    active_package.predecessor_revision_digest = None;

    let mut candidate = bind_package_plan(base.clone(), package.clone());
    candidate.digest.clear();
    candidate
        .package
        .as_mut()
        .expect("candidate package")
        .predecessor_revision_digest = None;
    active == candidate
}

pub(crate) async fn prepare_loaded_graph_package_install(
    access: &ConfigAccess,
    package: &LoadedGraphPackage,
    options: &GraphPackageInstallBindings,
) -> anyhow::Result<PreparedGraphPackageInstall> {
    prepare_package(access, package, options, None).await
}

async fn prepare_package(
    access: &ConfigAccess,
    package: &LoadedGraphPackage,
    options: &GraphPackageInstallBindings,
    graph_id: Option<&str>,
) -> anyhow::Result<PreparedGraphPackageInstall> {
    validate_owner(access, &options.node_did).await?;
    let preview = crate::pack::preview_pack_inference_bindings(
        access,
        &package.manifest,
        &options.node_did,
        &options.inference_slots,
    )
    .await?;
    let config = crate::pack::bind_pack_install_config(
        &package.manifest,
        &package.config,
        &preview.bindings,
    )?;
    let intent = selected_intent(&config.graph_intents, graph_id)?;
    if intent.node_did != options.node_did {
        return Err(GraphPackageInstallError::GraphOwnerMismatch.into());
    }
    let bundle = DesiredStateApplyPlan::from_pack_config(&config)?;
    let desired_state = DesiredStateApplyPlan::new(
        bundle
            .documents()
            .iter()
            .filter(|document| document.collection != Collection::Node)
            .cloned()
            .collect(),
    )?;
    let base = compile_graph(
        intent,
        &config.graph_capabilities,
        &options.node_did,
        &CompilerPolicy::default(),
    )?;
    super::catalog::verify_shipped_plan(&base, &|path| {
        package.asset(path).ok().map(<[u8]>::to_vec)
    })?;
    let mut artifacts = Vec::new();
    for document in desired_state.documents() {
        artifacts.push(PlannedPackageArtifact {
            collection: document.collection,
            logical_id: document.add[document.collection.unique_field()]
                .as_str()
                .ok_or(GraphPackageInstallError::ConfigLogicalIdMissing)?
                .to_owned(),
            content_digest: crate::pack::pack_artifact_document_digest(&document.add)?,
        });
    }
    artifacts.sort();
    let mut schema_digests = Vec::new();
    for path in &package.manifest.schemas {
        let collections = query::parse_sdl(package.asset_text(path)?)?;
        if collections.is_empty() {
            return Err(
                GraphPackageInstallError::SchemaWithoutCollection { path: path.clone() }.into(),
            );
        }
        let mut collection_contract_digests = BTreeMap::new();
        for collection in collections {
            let name = collection.name.clone();
            if collection_contract_digests
                .insert(
                    name,
                    collection_schema_contract_digest(&serde_json::to_value(collection)?)?,
                )
                .is_some()
            {
                return Err(GraphPackageInstallError::SchemaRepeatsCollection.into());
            }
        }
        schema_digests.push(RequiredSchemaDigest {
            namespace: path.clone(),
            digest: digest_bytes(package.asset(path)?),
            collection_contract_digests,
        });
    }
    schema_digests.sort();
    for entry in &base.entries {
        let Some(prepare) = entry.prepare.as_ref() else {
            continue;
        };
        for collection in &prepare.writes {
            if !schema_digests
                .iter()
                .any(|schema| schema.collection_contract_digests.contains_key(collection))
            {
                return Err(GraphPackageInstallError::EntryWritesUndeclared {
                    entry: entry.name.clone(),
                    collection: collection.clone(),
                    package: package.manifest.name.clone(),
                }
                .into());
            }
        }
    }
    let mut package_plan = PackagePlan {
        name: package.manifest.name.clone(),
        version: package.manifest.version.clone(),
        package_digest: package.package_digest.clone(),
        bundled_provenance: BundledProvenance {
            binary_version: env!("CARGO_PKG_VERSION").into(),
            build_commit: option_env!("VERGEN_GIT_SHA").unwrap_or("unknown").into(),
        },
        workspace_authority: config
            .graph_capabilities
            .iter()
            .filter_map(|capability| {
                capability
                    .workspace_authority
                    .map(|authority| (capability.capability_id.clone(), authority))
            })
            .collect(),
        predecessor_revision_digest: None,
        artifacts,
        required_schema_digests: schema_digests.clone(),
    };
    let active = active_revision_plan(access, &options.node_did, &intent.graph_id).await?;
    let same_active = active
        .as_ref()
        .is_some_and(|active| same_install_configuration(active, &base, &package_plan));
    package_plan.predecessor_revision_digest = active.as_ref().and_then(|active| {
        if same_active {
            active
                .package
                .as_ref()
                .and_then(|package| package.predecessor_revision_digest.clone())
        } else {
            Some(active.digest.clone())
        }
    });
    let plan = bind_package_plan(base, package_plan);
    if same_active
        && !active
            .as_ref()
            .is_some_and(|active| active.digest == plan.digest)
    {
        return Err(GraphPackageInstallError::IdenticalInstallDiverged.into());
    }
    let desired = &desired_state;
    access
        .transact_readonly("graph_package.install_preflight", |txn| {
            Box::pin(async move {
                let effective =
                    crate::pack::prepare_pack_plan_in_txn(txn, desired.documents(), true).await?;
                crate::config_client::validate_desired_state_plan(txn, &effective).await?;
                Ok::<_, GraphPackageInstallError>(())
            })
        })
        .await?;
    Ok(PreparedGraphPackageInstall {
        plan,
        desired_state,
        schema_digests,
        expected_active_revision_digest: active.map(|active| active.digest),
    })
}

async fn ensure_package_schemas(
    access: &ConfigAccess,
    package: &LoadedGraphPackage,
) -> anyhow::Result<()> {
    let mut plans = Vec::new();
    for path in &package.manifest.schemas {
        let sdl = package.asset_text(path)?;
        let plan = crate::config_client::preview_schema_install(access, sdl)
            .await
            .with_context(|| format!("preview package schema {path:?}"))?;
        plans.push((path, plan));
    }
    for (path, plan) in plans {
        crate::config_client::apply_schema_install(
            access,
            package.asset_text(path)?,
            &plan.artifact_digest,
        )
        .await
        .with_context(|| format!("publish package schema {path:?}"))?;
    }
    Ok(())
}

pub async fn install_loaded_graph_package(
    access: &ConfigAccess,
    actor_did: &str,
    package: &LoadedGraphPackage,
    options: &GraphPackageInstallBindings,
    graph_id: Option<&str>,
    record: &GraphInstallRecord,
) -> anyhow::Result<GraphPackageInstallReceipt> {
    if actor_did != options.node_did {
        return Err(GraphPackageInstallError::ActorNotOwner.into());
    }
    let prepared = prepare_package(access, package, options, graph_id).await?;
    commit_prepared_graph_package_install(access, &options.node_did, package, &prepared, record)
        .await
}

async fn commit_prepared_graph_package_install(
    access: &ConfigAccess,
    owner: &str,
    package: &LoadedGraphPackage,
    prepared: &PreparedGraphPackageInstall,
    record: &GraphInstallRecord,
) -> anyhow::Result<GraphPackageInstallReceipt> {
    ensure_package_schemas(access, package).await?;
    let desired = &prepared.desired_state;
    let plan = &prepared.plan;
    let graph_id = &plan.graph_id;
    let expected_active_revision_digest = &prepared.expected_active_revision_digest;
    let identity = crate::pack::PackIdentity::new(
        &package.manifest,
        &package.package_digest,
        record.plugins.clone(),
    );
    let identity = &identity;
    access
        .transact_write("graph_package.install", |txn| {
            Box::pin(async move {
                let live_active_revision_digest = active_revision_digest(
                    &txn.execute(&active_revision_digest_query(owner, graph_id))
                        .await?,
                )?;
                if live_active_revision_digest != *expected_active_revision_digest {
                    return Err(anyhow::Error::new(crate::config_client::StaleExpectation {
                        drifted: vec![crate::config_client::DriftedDocument {
                            collection: Collection::GraphDefinition,
                            owner: owner.to_owned(),
                            id: graph_id.clone(),
                            expected: expected_active_revision_digest.clone(),
                            found: live_active_revision_digest,
                        }],
                    }));
                }
                let effective =
                    crate::pack::prepare_pack_plan_in_txn(txn, desired.documents(), true).await?;
                crate::config_client::validate_desired_state_plan(txn, &effective).await?;
                let observed = crate::pack::observe_graph_install_in_txn(
                    txn,
                    owner,
                    &identity.coordinate,
                    desired.documents(),
                    graph_id,
                )
                .await?;
                apply_desired_state_plan(txn, &effective).await?;
                crate::graph_pipeline::materialize_graph_revision_in_txn(txn, owner, plan).await?;
                crate::pack::record_graph_install_in_txn(
                    txn,
                    owner,
                    identity,
                    record.explicit,
                    observed,
                    desired.documents(),
                    graph_id,
                    &package.manifest.name,
                )
                .await?;
                Ok(())
            })
        })
        .await?;
    let package = prepared.plan.package.as_ref().expect("bound package plan");
    Ok(GraphPackageInstallReceipt {
        package_name: package.name.clone(),
        package_version: package.version.clone(),
        package_digest: package.package_digest.clone(),
        graph_id: prepared.plan.graph_id.clone(),
        revision_digest: prepared.plan.digest.clone(),
        predecessor_revision_digest: package.predecessor_revision_digest.clone(),
        artifacts_complete: true,
        desired_documents: prepared.desired_state.documents().len(),
        schema_digests: prepared.schema_digests.clone(),
    })
}

#[cfg(test)]
mod tests;
