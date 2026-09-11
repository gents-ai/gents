use super::{load_bundled_graph_package, BundledGraphPackage};
use crate::config_client::{
    apply_desired_state_plan, collection_schema_contract_digest, ConfigAccess,
    DesiredStateApplyPlan,
};
use crate::graph_pipeline::{
    bind_package_plan, compile_graph, BundledProvenance, CompilerPolicy, GraphIntent, GraphPlan,
    PackagePlan, PlannedPackageArtifact, RequiredSchemaDigest,
};
use crate::Collection;
use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub type GraphPackageInstallBindings = crate::pack::PackInstallOptions;

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
}

fn selected_intent<'a>(
    package: &'a BundledGraphPackage,
    graph_id: Option<&str>,
) -> Result<&'a GraphIntent> {
    if let Some(graph_id) = graph_id {
        let mut matches = package
            .config
            .graph_intents
            .iter()
            .filter(|intent| intent.graph_id == graph_id);
        let selected = matches.next().context("selected graph is not in package")?;
        anyhow::ensure!(
            matches.next().is_none(),
            "selected graph is ambiguous in package"
        );
        return Ok(selected);
    }
    match package.config.graph_intents.as_slice() {
        [intent] => Ok(intent),
        [] => anyhow::bail!("package contains no graph intent"),
        _ => anyhow::bail!(
            "package contains multiple graphs; use the explicit graph selection entry point"
        ),
    }
}

async fn validate_owner(access: &ConfigAccess, owner: &str) -> Result<()> {
    anyhow::ensure!(
        !owner.trim().is_empty(),
        "package owner DID must not be blank"
    );
    access
        .transact("graph_package.owner", |txn| {
            Box::pin(async move {
                let principal = crate::config_client::read_desired_state_document_in_txn(
                    txn,
                    Collection::AgentPrincipal,
                    owner,
                    owner,
                )
                .await?
                .context("package owner principal is missing")?;
                anyhow::ensure!(
                    principal["enabled"].as_bool() == Some(true),
                    "package owner principal is disabled"
                );
                Ok(())
            })
        })
        .await
}

/// Select an existing principal without inventing host, model, or role defaults.
pub async fn default_bundled_graph_package_install_bindings(
    access: &ConfigAccess,
    package_name: &str,
    owner_did: &str,
) -> Result<GraphPackageInstallBindings> {
    let options = GraphPackageInstallBindings {
        agent_did: owner_did.to_owned(),
    };
    load_bundled_graph_package(package_name, &options)?;
    validate_owner(access, owner_did).await?;
    Ok(options)
}

/// Select installed package state without re-reading installation environment.
/// The runtime plan reader retains principal, revision and digest verification.
pub async fn load_installed_package_plan(
    access: &ConfigAccess,
    package_name: &str,
    owner_did: &str,
) -> Result<Option<GraphPlan>> {
    access
        .transact("graph_package.select_installed", move |txn| {
            Box::pin(async move {
                let response = txn
                    .execute(&format!(
                "{{ GraphDefinition(filter: {{agent_did: {{_eq: \"{}\"}}}}) {{graph_id}} }}",
                crate::graphql::escape_graphql_string(owner_did),
            ))
                    .await?;
                let mut selected = None;
                for definition in response_rows(&response, "GraphDefinition") {
                    let graph_id = definition["graph_id"]
                        .as_str()
                        .context("owned graph definition missing graph_id")?;
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
                        anyhow::ensure!(
                            selected.is_none(),
                            "multiple installed graphs belong to package {package_name:?}"
                        );
                        selected = Some(plan);
                    }
                }
                Ok(selected)
            })
        })
        .await
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

async fn active_revision_plan(
    access: &ConfigAccess,
    owner: &str,
    graph_id: &str,
) -> Result<Option<GraphPlan>> {
    let escaped_owner = crate::graphql::escape_graphql_string(owner);
    let response = access.execute(&format!(r#"{{ GraphDefinition(filter: {{ agent_did: {{ _eq: "{escaped_owner}" }}, graph_id: {{ _eq: "{}" }} }}, limit: 2) {{ active_revision_digest }} }}"#,
        crate::graphql::escape_graphql_string(graph_id))).await?;
    let definitions = response_rows(&response, "GraphDefinition");
    anyhow::ensure!(
        definitions.len() <= 1,
        "package graph definition is ambiguous within owner"
    );
    let Some(digest) = definitions
        .first()
        .and_then(|row| row["active_revision_digest"].as_str())
        .filter(|digest| !digest.is_empty())
    else {
        return Ok(None);
    };
    let response = access.execute(&format!(r#"{{ GraphRevision(filter: {{ owner_did: {{ _eq: "{escaped_owner}" }}, graph_id: {{ _eq: "{}" }}, digest: {{ _eq: "{}" }} }}, limit: 2) {{ plan_json }} }}"#,
        crate::graphql::escape_graphql_string(graph_id), crate::graphql::escape_graphql_string(digest))).await?;
    let revisions = response_rows(&response, "GraphRevision");
    anyhow::ensure!(
        revisions.len() == 1,
        "active package revision is missing or ambiguous"
    );
    let plan: GraphPlan = serde_json::from_str(
        revisions[0]["plan_json"]
            .as_str()
            .context("active revision missing plan_json")?,
    )?;
    anyhow::ensure!(
        plan.graph_id == graph_id
            && plan.digest == digest
            && crate::graph_pipeline::verify_graph_plan_digest(&plan),
        "active package revision failed immutable identity verification"
    );
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

pub async fn prepare_bundled_graph_package_install(
    access: &ConfigAccess,
    package_name: &str,
    options: &GraphPackageInstallBindings,
) -> Result<PreparedGraphPackageInstall> {
    let package = load_bundled_graph_package(package_name, options)?;
    prepare_package(access, &package, options, None).await
}

/// Explicit selection for packs containing more than one authored graph.
pub async fn prepare_bundled_graph_package_install_for_graph(
    access: &ConfigAccess,
    package_name: &str,
    options: &GraphPackageInstallBindings,
    graph_id: &str,
) -> Result<PreparedGraphPackageInstall> {
    let package = load_bundled_graph_package(package_name, options)?;
    prepare_package(access, &package, options, Some(graph_id)).await
}

async fn prepare_package(
    access: &ConfigAccess,
    package: &BundledGraphPackage,
    options: &GraphPackageInstallBindings,
    graph_id: Option<&str>,
) -> Result<PreparedGraphPackageInstall> {
    validate_owner(access, &options.agent_did).await?;
    let intent = selected_intent(package, graph_id)?;
    anyhow::ensure!(
        intent.agent_did == options.agent_did,
        "graph owner differs from installation scope"
    );
    // The existing principal is shared identity, never graph-owned replacement
    // configuration. Every other authored document uses the ordinary apply owner.
    let bundle = DesiredStateApplyPlan::from_pack_config(&package.config)?;
    let desired_state = DesiredStateApplyPlan::new(
        bundle
            .documents()
            .iter()
            .filter(|document| document.collection != Collection::AgentPrincipal)
            .cloned()
            .collect(),
    )?;
    let base = compile_graph(
        intent,
        &package.config.graph_capabilities,
        &options.agent_did,
        &CompilerPolicy::default(),
    )?;
    let mut artifacts = Vec::new();
    for document in desired_state.documents() {
        artifacts.push(PlannedPackageArtifact {
            collection: document.collection,
            logical_id: document.add[document.collection.unique_field()]
                .as_str()
                .context("configuration logical ID missing")?
                .to_owned(),
            content_digest: crate::config_client::desired_state_document_digest(&document.add)?,
        });
    }
    artifacts.sort();
    let mut schema_digests = Vec::new();
    for path in &package.manifest.schemas {
        let collections = query::parse_sdl(package.asset_text(path)?)?;
        anyhow::ensure!(
            !collections.is_empty(),
            "package schema {path:?} declares no collection"
        );
        let mut collection_contract_digests = BTreeMap::new();
        for collection in collections {
            let name = collection.name.clone();
            anyhow::ensure!(
                collection_contract_digests
                    .insert(
                        name,
                        collection_schema_contract_digest(&serde_json::to_value(collection)?)?
                    )
                    .is_none(),
                "package schema repeats a collection"
            );
        }
        schema_digests.push(RequiredSchemaDigest {
            namespace: path.clone(),
            digest: digest_bytes(package.asset(path)?),
            collection_contract_digests,
        });
    }
    schema_digests.sort();
    let mut package_plan = PackagePlan {
        name: package.manifest.name.clone(),
        version: package.manifest.version.clone(),
        package_digest: package.package_digest.clone(),
        bundled_provenance: BundledProvenance {
            binary_version: env!("CARGO_PKG_VERSION").into(),
            build_commit: option_env!("VERGEN_GIT_SHA").unwrap_or("unknown").into(),
        },
        workspace_authority: package
            .config
            .graph_capabilities
            .iter()
            .filter_map(|capability| {
                capability
                    .workspace_authority
                    .clone()
                    .map(|authority| (capability.capability_id.clone(), authority))
            })
            .collect(),
        predecessor_revision_digest: None,
        artifacts,
        required_schema_digests: schema_digests.clone(),
    };
    let active = active_revision_plan(access, &options.agent_did, &intent.graph_id).await?;
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
    anyhow::ensure!(
        !same_active
            || active
                .as_ref()
                .is_some_and(|active| active.digest == plan.digest),
        "identical package installation did not reproduce active revision"
    );
    let desired = &desired_state;
    access
        .transact("graph_package.install_preflight", |txn| {
            Box::pin(async move {
                crate::config_client::verify_existing_desired_state_plan(txn, desired).await?;
                crate::config_client::validate_desired_state_plan(txn, desired).await
            })
        })
        .await?;
    Ok(PreparedGraphPackageInstall {
        plan,
        desired_state,
        schema_digests,
    })
}

async fn ensure_package_schemas(
    access: &ConfigAccess,
    package: &BundledGraphPackage,
) -> Result<()> {
    // Check every already-visible contract before any additive schema write.
    let mut missing_paths = Vec::new();
    for path in &package.manifest.schemas {
        let expected = query::parse_sdl(package.asset_text(path)?)?;
        anyhow::ensure!(
            !expected.is_empty(),
            "package schema {path:?} declares no collection"
        );
        let mut missing = false;
        let mut existing = false;
        for collection in &expected {
            match access.collection_version(&collection.name).await? {
                Some(live) => {
                    existing = true;
                    anyhow::ensure!(
                        collection_schema_contract_digest(&serde_json::to_value(collection)?)?
                            == collection_schema_contract_digest(&live)?,
                        "existing collection {:?} does not match bundled schema {path:?}",
                        collection.name
                    );
                }
                None => missing = true,
            }
        }
        anyhow::ensure!(
            !(missing && existing),
            "package schema {path:?} mixes existing and missing collections"
        );
        if missing {
            missing_paths.push(path);
        }
    }
    for path in missing_paths {
        let sdl = package.asset_text(path)?;
        access
            .add_schema(sdl)
            .await
            .with_context(|| format!("add bundled package schema {path:?}"))?;
        for collection in query::parse_sdl(sdl)? {
            let live = access
                .collection_version(&collection.name)
                .await?
                .with_context(|| {
                    format!(
                        "new bundled collection {:?} is not discoverable",
                        collection.name
                    )
                })?;
            anyhow::ensure!(
                collection_schema_contract_digest(&serde_json::to_value(&collection)?)?
                    == collection_schema_contract_digest(&live)?,
                "new collection {:?} does not match bundled schema {path:?}",
                collection.name
            );
        }
    }
    Ok(())
}

pub async fn install_bundled_graph_package(
    access: &ConfigAccess,
    actor_did: &str,
    package_name: &str,
    options: &GraphPackageInstallBindings,
) -> Result<GraphPackageInstallReceipt> {
    install_package(access, actor_did, package_name, options, None).await
}

pub async fn install_bundled_graph_package_for_graph(
    access: &ConfigAccess,
    actor_did: &str,
    package_name: &str,
    options: &GraphPackageInstallBindings,
    graph_id: &str,
) -> Result<GraphPackageInstallReceipt> {
    install_package(access, actor_did, package_name, options, Some(graph_id)).await
}

async fn install_package(
    access: &ConfigAccess,
    actor_did: &str,
    package_name: &str,
    options: &GraphPackageInstallBindings,
    graph_id: Option<&str>,
) -> Result<GraphPackageInstallReceipt> {
    let package = load_bundled_graph_package(package_name, options)?;
    install_loaded_graph_package(access, actor_did, &package, options, graph_id).await
}

/// Publication owner shared by named distributions and already resolved packs.
pub(crate) async fn install_loaded_graph_package(
    access: &ConfigAccess,
    actor_did: &str,
    package: &BundledGraphPackage,
    options: &GraphPackageInstallBindings,
    graph_id: Option<&str>,
) -> Result<GraphPackageInstallReceipt> {
    anyhow::ensure!(
        actor_did == options.agent_did,
        "package install requires graph owner authority"
    );
    let prepared = prepare_package(access, package, options, graph_id).await?;
    ensure_package_schemas(access, package).await?;
    let desired = &prepared.desired_state;
    let owner = options.agent_did.as_str();
    let plan = &prepared.plan;
    access
        .transact("graph_package.install", |txn| {
            Box::pin(async move {
                crate::config_client::verify_existing_desired_state_plan(txn, desired).await?;
                apply_desired_state_plan(txn, desired).await?;
                crate::graph_pipeline::materialize_graph_revision_in_txn(txn, owner, plan).await?;
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
        schema_digests: prepared.schema_digests,
    })
}

#[cfg(test)]
mod tests;
