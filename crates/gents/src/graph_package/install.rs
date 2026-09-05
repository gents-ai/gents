use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::config_client::{
    apply_desired_state_plan, collection_schema_contract_digest, ConfigAccess,
    DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use crate::graph_pipeline::{
    bind_package_plan, compile_graph, BundledProvenance, CompilerPolicy, GraphPlan,
    PackageArtifactKind, PackagePlan, PackageRoleBinding, PlannedPackageArtifact,
    RequiredSchemaDigest, StageCapability,
};
use crate::{Collection, ToolSelectionDocument};

use super::{load_bundled_graph_package, BundledGraphPackage};

// Package-owned configuration is immutable input to the revision digest. A
// fixed timestamp keeps an identical install byte-for-byte idempotent while
// GraphRevision records retain the real installation/materialization time.
const PACKAGE_DOCUMENT_TIMESTAMP: &str = "1970-01-01T00:00:00Z";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphPackageInstallBindings {
    pub owner_did: String,
    pub roles: BTreeMap<String, PackageRoleBinding>,
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
}

/// Quickstart binding: bind every logical package role to the initialized
/// home's existing principal, local deployment, and default behavior model
/// configuration. This creates no principal or tool grant.
pub async fn default_bundled_graph_package_install_bindings(
    access: &ConfigAccess,
    package_name: &str,
    owner_did: &str,
) -> Result<GraphPackageInstallBindings> {
    let package = load_bundled_graph_package(package_name)?;
    if let ConfigAccess::Local(node) = access {
        crate::callback::ensure_local_host_deployment(node).await?;
    }
    let principal_response = access
        .execute(&format!(
            r#"{{ AgentPrincipal(filter: {{ agent_did: {{ _eq: "{}" }} }}, limit: 2) {{ agent_did enabled default_behavior_id }} }}"#,
            crate::graphql::escape_graphql_string(owner_did),
        ))
        .await?;
    let principals = response_rows(&principal_response, "AgentPrincipal");
    if principals.len() != 1 {
        anyhow::bail!("package owner principal is missing or ambiguous");
    }
    let principal = &principals[0];
    if principal.get("enabled").and_then(Value::as_bool) != Some(true) {
        anyhow::bail!("package owner principal is disabled");
    }
    let behavior_id = principal
        .get("default_behavior_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("package owner has no default behavior")?;
    let behavior_response = access
        .execute(&format!(
            r#"{{ AgentBehavior(filter: {{ behavior_id: {{ _eq: "{}" }} }}, limit: 2) {{ agent_did enabled backend_id inference_profile_id model_name }} }}"#,
            crate::graphql::escape_graphql_string(behavior_id),
        ))
        .await?;
    let behaviors = response_rows(&behavior_response, "AgentBehavior");
    if behaviors.len() != 1 {
        anyhow::bail!("package owner default behavior is missing or ambiguous");
    }
    let behavior = &behaviors[0];
    if behavior.get("enabled").and_then(Value::as_bool) != Some(true)
        || behavior.get("agent_did").and_then(Value::as_str) != Some(owner_did)
    {
        anyhow::bail!("package owner default behavior is disabled or cross-owner");
    }
    let deployment_response = access
        .execute(r#"{ HostDeployment(order: { created_at: ASC }, limit: 8) { deployment_id } }"#)
        .await?;
    let deployments = response_rows(&deployment_response, "HostDeployment");
    if deployments.len() != 1 {
        anyhow::bail!(
            "default package binding requires exactly one unambiguous local HostDeployment"
        );
    }
    let deployment_id = deployments
        .first()
        .and_then(|row| row.get("deployment_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .context("the running server has no local host deployment")?
        .to_owned();
    let role = PackageRoleBinding {
        principal_did: owner_did.to_owned(),
        deployment_id,
        backend_id: behavior
            .get("backend_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        profile_id: behavior
            .get("inference_profile_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        model_name: behavior
            .get("model_name")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
    };
    let roles = package
        .manifest
        .roles
        .iter()
        .map(|declared| (declared.name.clone(), role.clone()))
        .collect();
    let bindings = GraphPackageInstallBindings {
        owner_did: owner_did.to_owned(),
        roles,
    };
    validate_bindings(&package, &bindings)?;
    Ok(bindings)
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

fn component_hash(value: &str) -> String {
    let digest = format!("{:x}", Sha256::digest(value.as_bytes()));
    digest[..16].to_owned()
}

fn physical_id(prefix: &str, logical_id: &str) -> String {
    format!("pkg-{prefix}-{}", component_hash(logical_id))
}

pub fn bundled_graph_id(package_name: &str, owner_did: &str) -> Result<String> {
    let package = load_bundled_graph_package(package_name)?;
    Ok(format!(
        "graph-{}-{}",
        package.manifest.name,
        component_hash(owner_did)
    ))
}

fn interpolate_defaults(raw: &str) -> Result<String> {
    let with_default = Regex::new(r"\$\{[A-Z0-9_]+:-([^}]*)\}").expect("static regex");
    let rendered = with_default.replace_all(raw, "$1").into_owned();
    let unresolved = Regex::new(r"\$\{[^}]+\}").expect("static regex");
    if let Some(found) = unresolved.find(&rendered) {
        anyhow::bail!(
            "bundled asset contains unresolved placeholder {:?}",
            found.as_str()
        );
    }
    Ok(rendered)
}

fn parse_asset_object(package: &BundledGraphPackage, path: &str) -> Result<Map<String, Value>> {
    let rendered = interpolate_defaults(package.asset_text(path)?)?;
    serde_json::from_str::<Value>(&rendered)?
        .as_object()
        .cloned()
        .with_context(|| format!("package asset {path:?} must be a JSON object"))
}

fn object_string<'a>(object: &'a Map<String, Value>, field: &str) -> Result<&'a str> {
    object
        .get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("package resource is missing string field {field:?}"))
}

fn package_configuration_prefix(
    package: &BundledGraphPackage,
    bindings: &GraphPackageInstallBindings,
) -> Result<String> {
    let canonical = serde_json::to_vec(&json!({
        "package_digest": package.package_digest,
        "owner_did": bindings.owner_did,
        "roles": bindings.roles,
    }))?;
    Ok(format!(
        "{}-{}",
        package.manifest.name,
        &format!("{:x}", Sha256::digest(canonical))[..16]
    ))
}

fn validate_bindings(
    package: &BundledGraphPackage,
    bindings: &GraphPackageInstallBindings,
) -> Result<()> {
    if bindings.owner_did.trim().is_empty() {
        anyhow::bail!("package owner DID must not be empty");
    }
    let declared = package
        .manifest
        .roles
        .iter()
        .map(|role| role.name.as_str())
        .collect::<BTreeSet<_>>();
    let supplied = bindings
        .roles
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if declared != supplied {
        anyhow::bail!("package role bindings must exactly match declared logical roles");
    }
    for (name, role) in &bindings.roles {
        if role.principal_did.trim().is_empty()
            || role.deployment_id.trim().is_empty()
            || role.backend_id.as_deref().is_none_or(str::is_empty)
            || role.profile_id.as_deref().is_none_or(str::is_empty)
            || role.model_name.as_deref().is_none_or(str::is_empty)
        {
            anyhow::bail!(
                "role {name:?} requires an approved principal, deployment, backend, profile, and model"
            );
        }
        if role.principal_did != bindings.owner_did {
            anyhow::bail!("graph package v1 role {name:?} must bind the package owner principal");
        }
    }
    let deployments = bindings
        .roles
        .values()
        .map(|role| role.deployment_id.as_str())
        .collect::<BTreeSet<_>>();
    if deployments.len() != 1 {
        anyhow::bail!("graph package v1 roles must share one host deployment");
    }
    Ok(())
}

async fn require_live_binding(
    access: &ConfigAccess,
    collection: &str,
    field: &str,
    value: &str,
) -> Result<()> {
    crate::graphql::validate_collection_identifier(collection)?;
    crate::graphql::validate_graphql_name(field)?;
    let response = access
        .execute(&format!(
            r#"{{ {collection}(filter: {{ {field}: {{ _eq: "{}" }} }}, limit: 2) {{ _docID }} }}"#,
            crate::graphql::escape_graphql_string(value),
        ))
        .await?;
    let count = response_rows(&response, collection).len();
    if count != 1 {
        anyhow::bail!("package binding {collection}.{field}={value:?} is missing or ambiguous");
    }
    Ok(())
}

async fn validate_live_bindings(
    access: &ConfigAccess,
    bindings: &GraphPackageInstallBindings,
) -> Result<()> {
    require_live_binding(access, "AgentPrincipal", "agent_did", &bindings.owner_did).await?;
    for role in bindings.roles.values() {
        require_live_binding(access, "AgentPrincipal", "agent_did", &role.principal_did).await?;
        require_live_binding(
            access,
            "HostDeployment",
            "deployment_id",
            &role.deployment_id,
        )
        .await?;
        require_live_binding(
            access,
            "InferenceBackend",
            "backend_id",
            role.backend_id.as_deref().expect("validated"),
        )
        .await?;
        require_live_binding(
            access,
            "InferenceProfile",
            "profile_id",
            role.profile_id.as_deref().expect("validated"),
        )
        .await?;
    }
    Ok(())
}

fn desired_document(collection: Collection, add: Value, now: &str) -> DesiredStateApplyDocument {
    let mut update = add.clone();
    if let Some(object) = update.as_object_mut() {
        object.remove("created_at");
        if object.contains_key("updated_at") {
            object.insert("updated_at".to_owned(), Value::String(now.to_owned()));
        }
    }
    DesiredStateApplyDocument {
        collection,
        add,
        update,
    }
}

fn rewrite_surface(
    mut object: Map<String, Value>,
    principal_did: &str,
    id: &str,
    now: &str,
) -> Result<Value> {
    object.insert("surface_id".to_owned(), Value::String(id.to_owned()));
    object.insert(
        "agent_did".to_owned(),
        Value::String(principal_did.to_owned()),
    );
    let entries = object
        .get("entries")
        .and_then(Value::as_array)
        .context("DatastoreToolSurface entries must be an array")?
        .iter()
        .map(serde_json::to_string)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    object.insert(
        "entries".to_owned(),
        Value::Array(entries.into_iter().map(Value::String).collect()),
    );
    object.insert("created_at".to_owned(), Value::String(now.to_owned()));
    object.insert("updated_at".to_owned(), Value::String(now.to_owned()));
    Ok(Value::Object(object))
}

fn rewrite_selection(
    mut object: Map<String, Value>,
    principal_did: &str,
    id: &str,
    surface_ids: Vec<String>,
    now: &str,
) -> Result<Value> {
    object.insert("selection_id".to_owned(), Value::String(id.to_owned()));
    object.insert(
        "agent_did".to_owned(),
        Value::String(principal_did.to_owned()),
    );
    object.insert("file_tool_root".to_owned(), Value::Null);
    if object.get("enable_bash").and_then(Value::as_bool) == Some(true) {
        object.insert("bash_mode".to_owned(), Value::String("ReadOnly".to_owned()));
        object.insert(
            "command_execution_policy".to_owned(),
            Value::String("read_only".to_owned()),
        );
    }
    object.insert(
        "command_network_mode".to_owned(),
        Value::String("disabled".to_owned()),
    );
    object.insert(
        "datastore_tool_surface_ids".to_owned(),
        Value::Array(surface_ids.into_iter().map(Value::String).collect()),
    );
    object.insert("updated_at".to_owned(), Value::String(now.to_owned()));
    let value = Value::Object(object);
    let selection: ToolSelectionDocument = serde_json::from_value(value.clone())?;
    selection.validate()?;
    Ok(value)
}

fn rewrite_behavior(
    mut object: Map<String, Value>,
    role: &PackageRoleBinding,
    id: &str,
    selection_id: &str,
    system_prompt: &str,
    now: &str,
) -> Value {
    object.insert("behavior_id".to_owned(), Value::String(id.to_owned()));
    object.insert(
        "agent_did".to_owned(),
        Value::String(role.principal_did.clone()),
    );
    object.insert(
        "backend_id".to_owned(),
        Value::String(role.backend_id.clone().expect("validated")),
    );
    object.insert(
        "inference_profile_id".to_owned(),
        Value::String(role.profile_id.clone().expect("validated")),
    );
    object.insert(
        "model_name".to_owned(),
        Value::String(role.model_name.clone().expect("validated")),
    );
    object.insert(
        "tool_selection_id".to_owned(),
        Value::String(selection_id.to_owned()),
    );
    object.insert(
        "system_prompt".to_owned(),
        Value::String(system_prompt.to_owned()),
    );
    object.insert("created_at".to_owned(), Value::String(now.to_owned()));
    object.insert("updated_at".to_owned(), Value::String(now.to_owned()));
    Value::Object(object)
}

fn rewrite_task(
    mut object: Map<String, Value>,
    id: &str,
    behavior_id: &str,
    prompt_template: &str,
    now: &str,
) -> Value {
    object.insert("task_id".to_owned(), Value::String(id.to_owned()));
    object.insert(
        "behavior_id".to_owned(),
        Value::String(behavior_id.to_owned()),
    );
    object.insert(
        "prompt_template".to_owned(),
        Value::String(prompt_template.to_owned()),
    );
    object.insert("enabled".to_owned(), Value::Bool(true));
    object.insert("created_at".to_owned(), Value::String(now.to_owned()));
    object.insert("updated_at".to_owned(), Value::String(now.to_owned()));
    Value::Object(object)
}

fn artifact(
    logical_id: String,
    physical_id: String,
    kind: PackageArtifactKind,
    document: &Value,
) -> Result<PlannedPackageArtifact> {
    Ok(PlannedPackageArtifact {
        logical_id,
        physical_id,
        kind,
        content_digest: crate::config_client::desired_state_document_digest(document)?,
    })
}

async fn active_revision_plan(access: &ConfigAccess, graph_id: &str) -> Result<Option<GraphPlan>> {
    let response = access
        .execute(&format!(
            r#"{{ GraphDefinition(filter: {{ graph_id: {{ _eq: "{}" }} }}, limit: 1) {{ active_revision_digest }} }}"#,
            crate::graphql::escape_graphql_string(graph_id),
        ))
        .await?;
    let Some(digest) = response
        .get("data")
        .and_then(|data| data.get("GraphDefinition"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("active_revision_digest"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let response = access
        .execute(&format!(
            r#"{{ GraphRevision(filter: {{ digest: {{ _eq: "{}" }} }}, limit: 2) {{ plan_json }} }}"#,
            crate::graphql::escape_graphql_string(digest),
        ))
        .await?;
    let plans = response
        .get("data")
        .and_then(|data| data.get("GraphRevision"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if plans.len() != 1 {
        anyhow::bail!("active package revision is missing or ambiguous");
    }
    let plan: GraphPlan = serde_json::from_str(
        plans[0]
            .get("plan_json")
            .and_then(Value::as_str)
            .context("active package revision is missing plan_json")?,
    )?;
    if plan.graph_id != graph_id
        || plan.digest != digest
        || !crate::graph_pipeline::verify_graph_plan_digest(&plan)
    {
        anyhow::bail!("active package revision failed immutable identity verification");
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

pub async fn prepare_bundled_graph_package_install(
    access: &ConfigAccess,
    package_name: &str,
    bindings: &GraphPackageInstallBindings,
) -> Result<PreparedGraphPackageInstall> {
    let package = load_bundled_graph_package(package_name)?;
    validate_bindings(&package, bindings)?;
    validate_live_bindings(access, bindings).await?;
    let prefix = package_configuration_prefix(&package, bindings)?;
    let graph_id = bundled_graph_id(package_name, &bindings.owner_did)?;
    let active_plan = active_revision_plan(access, &graph_id).await?;
    let now = PACKAGE_DOCUMENT_TIMESTAMP;
    let mut documents = Vec::new();
    let mut artifacts = Vec::new();
    let mut capabilities = Vec::new();
    let mut ceilings = BTreeMap::new();

    for template in &package.capabilities {
        let role = bindings.roles.get(&template.role).expect("validated role");
        let behavior_object = parse_asset_object(&package, &template.behavior_asset)?;
        let logical_behavior_id = object_string(&behavior_object, "behavior_id")?.to_owned();
        let behavior_id = physical_id(&prefix, &logical_behavior_id);

        let selection_object = parse_asset_object(&package, &template.tool_selection_asset)?;
        let logical_selection_id = object_string(&selection_object, "selection_id")?.to_owned();
        let selection_id = physical_id(&prefix, &logical_selection_id);
        let mut surface_ids = Vec::new();
        for surface_asset in &template.tool_surface_assets {
            let surface_object = parse_asset_object(&package, surface_asset)?;
            let logical_surface_id = object_string(&surface_object, "surface_id")?.to_owned();
            let surface_id = physical_id(&prefix, &logical_surface_id);
            let surface = rewrite_surface(surface_object, &role.principal_did, &surface_id, now)?;
            artifacts.push(artifact(
                logical_surface_id,
                surface_id.clone(),
                PackageArtifactKind::ToolSurface,
                &surface,
            )?);
            documents.push(desired_document(
                Collection::DatastoreToolSurface,
                surface,
                now,
            ));
            surface_ids.push(surface_id);
        }
        let selection = rewrite_selection(
            selection_object,
            &role.principal_did,
            &selection_id,
            surface_ids,
            now,
        )?;
        artifacts.push(artifact(
            logical_selection_id,
            selection_id.clone(),
            PackageArtifactKind::ToolSelection,
            &selection,
        )?);
        documents.push(desired_document(Collection::ToolSelection, selection, now));

        let behavior = rewrite_behavior(
            behavior_object,
            role,
            &behavior_id,
            &selection_id,
            package.asset_text(&template.system_prompt_asset)?,
            now,
        );
        artifacts.push(artifact(
            logical_behavior_id,
            behavior_id.clone(),
            PackageArtifactKind::Behavior,
            &behavior,
        )?);
        documents.push(desired_document(Collection::AgentBehavior, behavior, now));

        let task_object = parse_asset_object(&package, &template.task_asset)?;
        let logical_task_id = object_string(&task_object, "task_id")?.to_owned();
        let task_id = physical_id(&prefix, &logical_task_id);
        let task = rewrite_task(
            task_object,
            &task_id,
            &behavior_id,
            package.asset_text(&template.task_prompt_asset)?,
            now,
        );
        artifacts.push(artifact(
            logical_task_id,
            task_id.clone(),
            PackageArtifactKind::Task,
            &task,
        )?);
        documents.push(desired_document(Collection::Task, task, now));

        capabilities.push(StageCapability {
            capability_id: template.capability_id.clone(),
            revision: template.revision.clone(),
            task_id,
            input_ports: template.input_ports.clone(),
            output_ports: template.output_ports.clone(),
            allowed_callers: vec![bindings.owner_did.clone()],
        });
        ceilings.insert(
            template.capability_id.clone(),
            template.workspace_authority.clone(),
        );
    }

    let mut intent = package.intent.clone();
    intent.graph_id = graph_id;
    let base_plan = compile_graph(
        &intent,
        &capabilities,
        &bindings.owner_did,
        &CompilerPolicy::default(),
    )?;
    let mut schema_digests = package
        .manifest
        .schemas
        .iter()
        .map(|path| {
            let collection_contract_digests = query::parse_sdl(package.asset_text(path)?)?
                .into_iter()
                .map(|collection| {
                    let name = collection.name.clone();
                    let version = serde_json::to_value(collection)?;
                    Ok((name, collection_schema_contract_digest(&version)?))
                })
                .collect::<Result<BTreeMap<_, _>>>()?;
            Ok(RequiredSchemaDigest {
                namespace: path.clone(),
                digest: digest_bytes(package.asset(path)?),
                collection_contract_digests,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    schema_digests.sort();
    let mut package_plan = PackagePlan {
        name: package.manifest.name.clone(),
        version: package.manifest.version.clone(),
        package_digest: package.package_digest.clone(),
        bundled_provenance: BundledProvenance {
            binary_version: env!("CARGO_PKG_VERSION").to_owned(),
            build_commit: option_env!("VERGEN_GIT_SHA")
                .unwrap_or("unknown")
                .to_owned(),
        },
        roles: bindings.roles.clone(),
        workspace_authority: ceilings,
        predecessor_revision_digest: None,
        artifacts,
        required_schema_digests: schema_digests.clone(),
    };
    package_plan.predecessor_revision_digest = active_plan
        .as_ref()
        .map(|active| {
            if same_install_configuration(active, &base_plan, &package_plan) {
                active
                    .package
                    .as_ref()
                    .and_then(|package| package.predecessor_revision_digest.clone())
            } else {
                Some(active.digest.clone())
            }
        })
        .flatten();
    let same_active = active_plan.as_ref().is_some_and(|active| {
        same_install_configuration(
            active,
            &base_plan,
            &PackagePlan {
                predecessor_revision_digest: None,
                ..package_plan.clone()
            },
        )
    });
    let plan = bind_package_plan(base_plan, package_plan);
    if same_active
        && active_plan
            .as_ref()
            .is_some_and(|active| active.digest != plan.digest)
    {
        anyhow::bail!("identical package installation did not reproduce its active revision");
    }
    Ok(PreparedGraphPackageInstall {
        plan,
        desired_state: DesiredStateApplyPlan::new(documents)?,
        schema_digests,
    })
}

async fn ensure_package_schemas(
    access: &ConfigAccess,
    package: &BundledGraphPackage,
) -> Result<()> {
    for path in &package.manifest.schemas {
        let sdl = package.asset_text(path)?;
        let expected = query::parse_sdl(sdl)?;
        if expected.is_empty() {
            anyhow::bail!("package schema {path:?} declares no collection");
        }
        let mut missing = false;
        let mut existing = false;
        for collection in &expected {
            match access.collection_version(&collection.name).await? {
                Some(live_version) => {
                    existing = true;
                    let expected_version = serde_json::to_value(collection)?;
                    let expected_digest = collection_schema_contract_digest(&expected_version)?;
                    let live_digest = collection_schema_contract_digest(&live_version)?;
                    if expected_digest != live_digest {
                        anyhow::bail!(
                            "existing collection {:?} does not match bundled schema {path:?}: expected {expected_digest}, found {live_digest}",
                            collection.name,
                        );
                    }
                }
                None => missing = true,
            }
        }
        if missing {
            if existing {
                anyhow::bail!("package schema {path:?} mixes existing and missing collections");
            }
            access
                .add_schema(sdl)
                .await
                .with_context(|| format!("add bundled package schema {path:?}"))?;
            for collection in &expected {
                let live = access
                    .collection_version(&collection.name)
                    .await?
                    .with_context(|| {
                        format!(
                            "bundled package schema {path:?} was accepted but collection {:?} is not discoverable",
                            collection.name
                        )
                    })?;
                let expected_version = serde_json::to_value(collection)?;
                let expected_digest = collection_schema_contract_digest(&expected_version)?;
                let live_digest = collection_schema_contract_digest(&live)?;
                if live_digest != expected_digest {
                    anyhow::bail!(
                        "new collection {:?} does not match bundled schema {path:?}: expected {expected_digest}, found {live_digest}",
                        collection.name,
                    );
                }
            }
        }
    }
    Ok(())
}

pub async fn install_bundled_graph_package(
    access: &ConfigAccess,
    actor_did: &str,
    package_name: &str,
    bindings: &GraphPackageInstallBindings,
) -> Result<GraphPackageInstallReceipt> {
    if actor_did != bindings.owner_did {
        anyhow::bail!("install authority is separate and currently requires the graph owner");
    }
    // Complete every read-only package, binding, and desired-state check
    // before adding globally visible schema. Schema registration remains an
    // additive, idempotent prerequisite; package artifacts and the immutable
    // revision are committed together below.
    let prepared = prepare_bundled_graph_package_install(access, package_name, bindings).await?;
    let preflight = access.begin_apply_txn().await?;
    let preflight_result = crate::config_client::verify_existing_desired_state_plan(
        &preflight,
        &prepared.desired_state,
    )
    .await;
    let discard_result = preflight.discard().await;
    preflight_result?;
    discard_result.context("discard graph package install preflight")?;

    let package = load_bundled_graph_package(package_name)?;
    ensure_package_schemas(access, &package).await?;

    let txn = access.begin_apply_txn().await?;
    let result = async {
        crate::config_client::verify_existing_desired_state_plan(&txn, &prepared.desired_state)
            .await?;
        apply_desired_state_plan(&txn, &prepared.desired_state).await?;
        crate::graph_pipeline::materialize_graph_revision_in_txn(
            &txn,
            &bindings.owner_did,
            &prepared.plan,
        )
        .await?;
        Result::<()>::Ok(())
    }
    .await;
    if let Err(error) = result {
        let _ = txn.discard().await;
        return Err(error);
    }
    txn.commit()
        .await
        .context("commit graph package installation")?;

    let package_plan = prepared.plan.package.as_ref().expect("bound package plan");
    Ok(GraphPackageInstallReceipt {
        package_name: package_plan.name.clone(),
        package_version: package_plan.version.clone(),
        package_digest: package_plan.package_digest.clone(),
        graph_id: prepared.plan.graph_id.clone(),
        revision_digest: prepared.plan.digest.clone(),
        predecessor_revision_digest: package_plan.predecessor_revision_digest.clone(),
        artifacts_complete: true,
        desired_documents: prepared.desired_state.documents().len(),
        schema_digests: prepared.schema_digests,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_pipeline::{activate_graph_revision, start_graph_run};
    use defra_node::EmbeddedNode;
    use std::sync::Arc;

    async fn create_fixture_bindings(node: &EmbeddedNode) -> GraphPackageInstallBindings {
        let owner = "did:key:package-owner";
        crate::document_config::ensure_agent_principal(node, owner)
            .await
            .unwrap();
        for mutation in [
            r#"mutation { create_HostDeployment(input: {
                deployment_id: "local-test", display_name: "Local test"
            }) { _docID } }"#,
            r#"mutation { create_InferenceBackend(input: {
                backend_id: "test-backend", name: "Test", provider_kind: "OpenAiCompatible",
                endpoint: "http://127.0.0.1:1/v1", max_concurrent: 4,
                enabled: true, models: ["test-model"]
            }) { _docID } }"#,
            r#"mutation { create_InferenceProfile(input: {
                profile_id: "test-profile", display_name: "Test", max_turns: 8
            }) { _docID } }"#,
        ] {
            let response = node.execute(mutation).await;
            assert!(!response.has_errors(), "{:?}", response.errors);
        }
        let role = PackageRoleBinding {
            principal_did: owner.to_owned(),
            deployment_id: "local-test".to_owned(),
            backend_id: Some("test-backend".to_owned()),
            profile_id: Some("test-profile".to_owned()),
            model_name: Some("test-model".to_owned()),
        };
        GraphPackageInstallBindings {
            owner_did: owner.to_owned(),
            roles: BTreeMap::from([
                ("coordinator".to_owned(), role.clone()),
                ("reviewer".to_owned(), role),
            ]),
        }
    }

    #[tokio::test]
    async fn live_binding_failure_does_not_register_package_schema() {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        let access = ConfigAccess::Local(node.clone());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let mut bindings = create_fixture_bindings(node.as_ref()).await;
        for role in bindings.roles.values_mut() {
            role.backend_id = Some("missing-backend".to_owned());
        }

        let error =
            install_bundled_graph_package(&access, &bindings.owner_did, "code-review", &bindings)
                .await
                .unwrap_err();
        assert!(error.to_string().contains("missing or ambiguous"));
        assert!(node.get_collection("CodeReviewJob").unwrap().is_none());

        drop(access);
        let node = Arc::try_unwrap(node).unwrap_or_else(|_| panic!("test retained node clone"));
        node.shutdown().await;
    }

    #[tokio::test]
    async fn install_rejects_a_model_the_bound_backend_does_not_advertise() {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        let access = ConfigAccess::Local(node.clone());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let mut bindings = create_fixture_bindings(node.as_ref()).await;
        for role in bindings.roles.values_mut() {
            role.model_name = Some("not-advertised".to_owned());
        }

        let error =
            install_bundled_graph_package(&access, &bindings.owner_did, "code-review", &bindings)
                .await
                .expect_err("package behavior must use a model advertised by its backend");
        assert!(
            format!("{error:#}").contains("does not advertise"),
            "{error:#}"
        );
        let state = node
            .execute("{ AgentBehavior { behavior_id model_name } GraphRevision { digest } }")
            .await;
        assert!(!state.has_errors(), "{:?}", state.errors);
        let data = state.data.unwrap();
        assert!(data["AgentBehavior"]
            .as_array()
            .unwrap()
            .iter()
            .all(|behavior| {
                !behavior["behavior_id"]
                    .as_str()
                    .is_some_and(|id| id.starts_with("pkg-"))
                    && behavior["model_name"] != "not-advertised"
            }));
        assert!(data["GraphRevision"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn existing_package_schema_must_match_types_indexes_and_immutability() {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        let access = ConfigAccess::Local(node.clone());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let bindings = create_fixture_bindings(node.as_ref()).await;
        let package = load_bundled_graph_package("code-review").unwrap();
        let expected = package.asset_text("schemas/review_job.graphql").unwrap();
        let incompatible = expected.replace(
            "run_id: String @index(unique: true) @immutable",
            "run_id: Int",
        );
        assert_ne!(incompatible, expected);
        node.add_schema(&incompatible).await.unwrap();

        let error =
            install_bundled_graph_package(&access, &bindings.owner_did, "code-review", &bindings)
                .await
                .unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("does not match bundled schema"),
            "{message}"
        );
        assert!(node.get_collection("CodeReviewArea").unwrap().is_none());

        drop(access);
        let node = Arc::try_unwrap(node).unwrap_or_else(|_| panic!("test retained node clone"));
        node.shutdown().await;
    }

    #[tokio::test]
    async fn code_review_install_is_idempotent_shared_home_safe_and_runnable() {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        let access = ConfigAccess::Local(node.clone());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let bindings = create_fixture_bindings(&node).await;
        let unrelated = node
            .execute(
                r#"mutation { create_Task(input: {
                    task_id: "unrelated-task", name: "Unrelated",
                    behavior_id: "unrelated-behavior", prompt_template: "Keep me",
                    enabled: false
                }) { _docID } }"#,
            )
            .await;
        assert!(!unrelated.has_errors(), "{:?}", unrelated.errors);

        let first =
            install_bundled_graph_package(&access, &bindings.owner_did, "code-review", &bindings)
                .await
                .unwrap();
        let second =
            install_bundled_graph_package(&access, &bindings.owner_did, "code-review", &bindings)
                .await
                .unwrap();
        assert_eq!(first, second);
        assert_eq!(first.desired_documents, 16);

        let state = node
            .execute(
                r#"{
                    GraphRevision { digest artifacts_complete plan_json }
                    Task { task_id goal_objective_template goal_token_budget }
                    EventTrigger { trigger_id }
                }"#,
            )
            .await;
        assert!(!state.has_errors(), "{:?}", state.errors);
        let data = state.data.unwrap();
        assert_eq!(data["GraphRevision"].as_array().unwrap().len(), 1);
        assert_eq!(data["GraphRevision"][0]["artifacts_complete"], true);
        assert!(data["Task"]
            .as_array()
            .unwrap()
            .iter()
            .any(|task| task["task_id"] == "unrelated-task"));
        let review_tasks = data["Task"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|task| task["task_id"] != "unrelated-task")
            .collect::<Vec<_>>();
        assert_eq!(review_tasks.len(), 4);
        for task in review_tasks {
            assert!(task["goal_objective_template"]
                .as_str()
                .is_some_and(|s| !s.trim().is_empty()));
            assert!(task["goal_token_budget"]
                .as_i64()
                .is_some_and(|budget| budget > 0));
        }
        assert_eq!(
            data["EventTrigger"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|trigger| trigger["trigger_id"]
                    .as_str()
                    .is_some_and(|id| id.starts_with("graph-trigger-")))
                .count(),
            4
        );
        let inactive_view = crate::agent::load_document_runtime_view(&node, &bindings.owner_did)
            .await
            .unwrap();
        assert_eq!(
            inactive_view
                .behaviors
                .keys()
                .filter(|id| id.starts_with("pkg-"))
                .count(),
            0,
            "installed but unpublished package behavior must stay hidden"
        );

        activate_graph_revision(
            &node,
            None,
            &bindings.owner_did,
            &first.graph_id,
            &first.revision_digest,
            None,
        )
        .await
        .unwrap();
        let after_activation =
            install_bundled_graph_package(&access, &bindings.owner_did, "code-review", &bindings)
                .await
                .unwrap();
        assert_eq!(first, after_activation);
        let active_view = crate::agent::load_document_runtime_view(&node, &bindings.owner_did)
            .await
            .unwrap();
        assert_eq!(
            active_view
                .behaviors
                .keys()
                .filter(|id| id.starts_with("pkg-"))
                .count(),
            4
        );
        assert_eq!(
            active_view
                .datastore_tool_surfaces
                .keys()
                .filter(|id| id.starts_with("pkg-"))
                .count(),
            4,
            "active package datastore tool surfaces must be visible"
        );
        for selection in active_view
            .tool_selections
            .values()
            .filter(|record| record.value.selection_id.starts_with("pkg-"))
        {
            assert!(
                !selection
                    .value
                    .datastore_tool_surface_ids
                    .as_deref()
                    .unwrap_or_default()
                    .is_empty(),
                "package ToolSelection {} must retain its surface links",
                selection.value.selection_id
            );
        }
        let run = start_graph_run(
            &node,
            None,
            &bindings.owner_did,
            &first.graph_id,
            None,
            "review",
            json!({
                "repository_path": "/tmp/repo",
                "base_ref": "base-sha",
                "head_ref": "head-sha",
                "lens_count": "4",
                "lens_min": "4",
                "lens_max": "4",
                "focus": "durability"
            }),
        )
        .await
        .unwrap();
        assert_eq!(run.revision_digest, first.revision_digest);

        let profile = node
            .execute(
                r#"mutation { create_InferenceProfile(input: {
                    profile_id: "test-profile-successor", display_name: "Successor", max_turns: 12
                }) { _docID } }"#,
            )
            .await;
        assert!(!profile.has_errors(), "{:?}", profile.errors);
        let mut successor_bindings = bindings.clone();
        successor_bindings
            .roles
            .get_mut("reviewer")
            .unwrap()
            .profile_id = Some("test-profile-successor".to_owned());
        let successor = install_bundled_graph_package(
            &access,
            &bindings.owner_did,
            "code-review",
            &successor_bindings,
        )
        .await
        .unwrap();
        assert_ne!(successor.revision_digest, first.revision_digest);
        assert_eq!(
            successor.predecessor_revision_digest.as_deref(),
            Some(first.revision_digest.as_str())
        );
        let definition = node
            .execute("{ GraphDefinition { active_revision_digest generation } }")
            .await;
        assert!(!definition.has_errors(), "{:?}", definition.errors);
        assert_eq!(
            definition.data.as_ref().unwrap()["GraphDefinition"][0]["active_revision_digest"],
            first.revision_digest
        );
        let before_successor_publish =
            crate::agent::load_document_runtime_view(&node, &bindings.owner_did)
                .await
                .unwrap();
        assert_eq!(
            before_successor_publish
                .behaviors
                .keys()
                .filter(|id| id.starts_with("pkg-"))
                .count(),
            4,
            "unpublished successor resources must stay hidden"
        );
        activate_graph_revision(
            &node,
            None,
            &bindings.owner_did,
            &first.graph_id,
            &successor.revision_digest,
            Some(&first.revision_digest),
        )
        .await
        .unwrap();
        let pinned =
            crate::graph_pipeline::load_graph_run_view(&node, &bindings.owner_did, &run.run_id)
                .await
                .unwrap();
        assert_eq!(pinned.revision_digest, first.revision_digest);
        let pinned_and_active =
            crate::agent::load_document_runtime_view(&node, &bindings.owner_did)
                .await
                .unwrap();
        assert_eq!(
            pinned_and_active
                .behaviors
                .keys()
                .filter(|id| id.starts_with("pkg-"))
                .count(),
            8,
            "active and nonterminal-run-pinned revisions retain their own resources"
        );
        let next_run = start_graph_run(
            &node,
            None,
            &bindings.owner_did,
            &first.graph_id,
            None,
            "review",
            json!({
                "repository_path": "/tmp/repo",
                "base_ref": "base-sha",
                "head_ref": "head-sha",
                "lens_count": "4",
                "lens_min": "4",
                "lens_max": "4",
                "focus": "durability"
            }),
        )
        .await
        .unwrap();
        assert_eq!(next_run.revision_digest, successor.revision_digest);

        crate::graph_pipeline::request_graph_run_cancellation(
            &node,
            None,
            &bindings.owner_did,
            &run.run_id,
            Some("visibility test complete"),
        )
        .await
        .unwrap();
        let successor_only = crate::agent::load_document_runtime_view(&node, &bindings.owner_did)
            .await
            .unwrap();
        assert_eq!(
            successor_only
                .behaviors
                .keys()
                .filter(|id| id.starts_with("pkg-"))
                .count(),
            4,
            "terminal predecessor run must release its retired resources"
        );

        let revision = node
            .execute(&format!(
                r#"{{ GraphRevision(filter: {{ digest: {{ _eq: "{}" }} }}) {{ plan_json }} }}"#,
                crate::graphql::escape_graphql_string(&successor.revision_digest),
            ))
            .await;
        assert!(!revision.has_errors(), "{:?}", revision.errors);
        let successor_plan: GraphPlan = serde_json::from_str(
            revision.data.as_ref().unwrap()["GraphRevision"][0]["plan_json"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        let selection_id = successor_plan
            .package
            .as_ref()
            .unwrap()
            .artifacts
            .iter()
            .find(|artifact| artifact.kind == PackageArtifactKind::ToolSelection)
            .unwrap()
            .physical_id
            .clone();
        let drift = node
            .execute(&format!(
                r#"mutation {{
                    update_ToolSelection(
                        filter: {{ selection_id: {{ _eq: "{}" }} }},
                        input: {{ display_name: "drifted package selection" }}
                    ) {{ _docID }}
                }}"#,
                crate::graphql::escape_graphql_string(&selection_id),
            ))
            .await;
        assert!(!drift.has_errors(), "{:?}", drift.errors);
        let start_error = start_graph_run(
            &node,
            None,
            &bindings.owner_did,
            &first.graph_id,
            None,
            "review",
            json!({
                "repository_path": "/tmp/repo",
                "base_ref": "base-sha",
                "head_ref": "head-sha",
                "lens_count": "4",
                "lens_min": "4",
                "lens_max": "4",
                "focus": "drift"
            }),
        )
        .await
        .unwrap_err();
        assert!(format!("{start_error:#}").contains("drifted"));
        let retry_error = install_bundled_graph_package(
            &access,
            &bindings.owner_did,
            "code-review",
            &successor_bindings,
        )
        .await
        .unwrap_err();
        assert!(format!("{retry_error:#}").contains("drifted"));
        drop(access);
        let node = Arc::try_unwrap(node).unwrap_or_else(|_| panic!("test retained node clone"));
        node.shutdown().await;
    }
}
