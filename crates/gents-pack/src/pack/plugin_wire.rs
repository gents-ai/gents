//! The one writer of an installed plugin's place in an agent's Tools and
//! Context. Consent is the install's alone (the recorded grant bounds every
//! call), so wiring takes no authority flag.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use gents_documents::{
    apply_desired_state_plan, load_agent_context_and_tools_in_txn, DesiredStateApplyDocument,
    DesiredStateApplyPlan,
};
use gents_store::collection::Collection;
use gents_store::config_client::{
    ClientSigningCapability, ConfigAccess, ConfigApplyTxn, PackApprovalError, PackApprovalScope,
    PackEffectApprover, PackEvidence, PackMutation, PackRelationship, PackRelationshipEvidence,
};
use serde_json::{json, Value};

use crate::pack::installed_pack_documents;
use crate::pack::registry::split_pack_coordinate;
use crate::pack::resolved_pack::ResolvedPack;
use crate::plugin::authority::describe_plugin_authority;
use crate::plugin::store::read_record;

/// Loads the held User identity a write through a local transaction of the
/// given home signs with. A preview, or a write over GraphQL, never calls it,
/// so it never loads (or creates) that identity for them.
pub type LoadHeldUser = fn(
    &Path,
) -> futures::future::BoxFuture<
    'static,
    Result<Arc<dyn gents_store::identity::NodeIdentity>>,
>;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireRequest {
    pub plugin: String,
    pub agent: String,
    pub targets_from: Vec<String>,
    pub remove: bool,
    pub preview: bool,
    /// The node owner approved this plugin calling the agent's own tools
    /// (the init approval of its pack): its selection is wired with
    /// `tool_calls: true`. The wire command never sets it.
    pub tool_calls_consent: bool,
}

#[cfg(any(test, feature = "test-fixtures"))]
pub struct WireTestPause {
    pub home: std::path::PathBuf,
    pub reached: tokio::sync::Notify,
    pub release: tokio::sync::Notify,
}

/// Holds a wire of `home` after it takes the plugin store lock, until
/// `release`, so a test can contend with that lock.
#[cfg(any(test, feature = "test-fixtures"))]
pub static WIRE_TEST_PAUSE: std::sync::Mutex<Option<std::sync::Arc<WireTestPause>>> =
    std::sync::Mutex::new(None);

pub fn explain_tools_command(agent: &str) -> String {
    format!("gents config tools explain --agent-did {agent}")
}

/// Wires (or with `remove`, unwires) `request.plugin` over a held `access`.
/// A preview computes the same report and writes nothing. A new pin over
/// GraphQL is sent to the runtime serving that node ([`wire_remote`]), which
/// writes it through its own embedded store.
pub async fn wire_in(
    access: &ConfigAccess,
    owner: &str,
    home: &Path,
    request: &WireRequest,
    held_user: LoadHeldUser,
) -> Result<Value> {
    if let ConfigAccess::Graphql(endpoint) = access {
        if !request.preview && !request.remove {
            return wire_remote(endpoint, owner, request).await;
        }
    }
    wire_in_spawned(access, owner, home, request, held_user).await
}

/// [`wire_in`] over `access` itself, never sent to a runtime: what a runtime
/// serving a wire request runs, and what refuses a new pin over GraphQL.
pub async fn wire_in_spawned(
    access: &ConfigAccess,
    owner: &str,
    home: &Path,
    request: &WireRequest,
    held_user: LoadHeldUser,
) -> Result<Value> {
    let access = access.clone();
    let owner = owner.to_owned();
    let home = home.to_owned();
    let request = request.clone();
    tokio::spawn(async move { wire_in_owned(&access, &owner, &home, &request, held_user).await })
        .await?
}

pub fn wire_target_allowed(local: bool, preview: bool, remove: bool) -> bool {
    local || preview || remove
}

async fn wire_in_owned(
    access: &ConfigAccess,
    owner: &str,
    home: &Path,
    request: &WireRequest,
    held_user: LoadHeldUser,
) -> Result<Value> {
    anyhow::ensure!(wire_target_allowed(matches!(access, ConfigAccess::Local(_)), request.preview, request.remove),
            "cannot publish a plugin pin through GraphQL because the commit outcome may be unknown; stop the runtime serving the owning node home, then run `gents config pack wire {} --agent {} --home <node-home>` without --graphql",
            request.plugin, request.agent);
    let home_for_read = home.to_owned();
    let plugin_name = request.plugin.clone();
    let (_store_guard, plugin) = tokio::task::spawn_blocking(move || {
        let guard = crate::plugin::store::try_lock_store(&home_for_read)?;
        let (namespace, name) = split_pack_coordinate(&plugin_name);
        let plugin = read_record(&home_for_read, namespace, name).with_context(|| {
            format!("{plugin_name} is not installed. Next: gents config pack install <pack>")
        })?;
        Ok::<_, anyhow::Error>((guard, plugin))
    })
    .await??;
    #[cfg(any(test, feature = "test-fixtures"))]
    {
        let pause = WIRE_TEST_PAUSE
            .lock()
            .unwrap()
            .as_ref()
            .filter(|pause| pause.home == home)
            .cloned();
        if let Some(pause) = pause {
            pause.reached.notify_one();
            pause.release.notified().await;
        }
    }

    let mut plan = crate::pack::wiring::PluginWiring::read(
        access,
        owner,
        home,
        &request.agent,
        plugin.clone(),
        &request.targets_from,
    )
    .await?;
    plan.remove = request.remove;
    plan.preview = request.preview;
    plan.tool_calls_consent = request.tool_calls_consent;
    plan.next_step = explain_tools_command(&request.agent);
    let report = if request.preview {
        access
            .transact_readonly("cli.pack.wire.preview", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    plan.apply(txn, &gents_inference::resolved::ProviderProfilePolicy)
                        .await
                })
            })
            .await?
    } else if matches!(access, ConfigAccess::Local(_)) {
        let client = held_user(home).await?;
        anyhow::ensure!(
            access.actor_did().as_deref() == Some(client.did()),
            "pack wire requires the held User identity selected by the local transaction"
        );
        let scope = PackApprovalScope {
            node_did: owner.to_owned(),
            pack_digests: vec![plugin.digest.clone()],
            nonce: uuid::Uuid::new_v4().to_string(),
        };
        let approver = Arc::new(LocalWireApprover {
            actor: client.client_signing_capability()?,
            scope: scope.clone(),
        });
        access
            .transact_verified_pack_with_approver("cli.pack.wire", scope, approver, |txn| {
                let plan = &plan;
                Box::pin(async move {
                    plan.apply(
                        txn.approved_interpreter()?,
                        &gents_inference::resolved::ProviderProfilePolicy,
                    )
                    .await
                })
            })
            .await
            .map_err(|failure| {
                if failure.unconfirmed {
                    failure.error.context(
                        "pack wire commit was not confirmed; inspect the target before retrying",
                    )
                } else {
                    failure.error
                }
            })?
    } else {
        access
            .transact_write("cli.pack.wire", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    plan.apply(txn, &gents_inference::resolved::ProviderProfilePolicy)
                        .await
                })
            })
            .await?
    };

    Ok(json!({
        "plugin": request.plugin,
        "agent": request.agent,
        "preview": request.preview,
        "authority": describe_plugin_authority(&plugin.declaration, &plugin.ceiling()),
        "report": report,
    }))
}

/// Wires each plugin `pack` shipped into each of `agents`, agent by agent,
/// through the same writer as `gents config pack wire`.
pub async fn wire_pack_plugins(
    access: &ConfigAccess,
    owner: &str,
    home: &Path,
    pack: &ResolvedPack,
    agents: &[String],
    held_user: LoadHeldUser,
) -> Result<Vec<Value>> {
    wire_pack_batch(
        access,
        owner,
        home,
        pack,
        agents,
        WireBatchOptions::default(),
        held_user,
    )
    .await
}

/// Separate explicit operator transition. Wire/init never call this owner.
pub async fn enable_pack_dynamic(
    access: &gents_store::config_client::ConfigAccess,
    owner: &str,
    engineer: &str,
    source: &crate::pack::resolved_pack::ResolvedPack,
    home: &Path,
    held_user: LoadHeldUser,
) -> Result<Value> {
    if let ConfigAccess::Graphql(endpoint) = access {
        if access.actor_did().as_deref() == Some(owner) {
            return operator_remote(
                endpoint,
                owner,
                &PackOperatorAction::EnableDynamic {
                    digest: source.digest().to_owned(),
                    agent: engineer.to_owned(),
                },
            )
            .await;
        }
    }
    let config = crate::pack::load_pack_recorded_config(
        access,
        source.manifest(),
        &crate::pack::PackInstallOptions {
            node_did: owner.to_owned(),
        },
        &|path| Ok(source.asset(path)?.to_vec()),
        &|name| (name == "GENTS_PACK_NODE_DID").then(|| owner.to_owned()),
    )
    .await?;
    let jobs: Vec<_> = source
        .manifest()
        .metadata
        .plugins
        .iter()
        .filter(|p| p.job.0)
        .map(|p| {
            let coordinate = format!("{}/{}", source.manifest().metadata.namespace, p.name);
            let job = config
                .tools
                .iter()
                .flat_map(|t| t.integrations.iter())
                .flat_map(|i| i.plugins.iter().flatten())
                .find(|p| p.plugin == coordinate)
                .and_then(|p| p.job.as_ref())
                .with_context(|| format!("{coordinate} has no pack job targets"))?;
            Ok((coordinate, job.policy.dynamic_targets.clone()))
        })
        .collect::<Result<_>>()?;
    anyhow::ensure!(!jobs.is_empty(), "Dynamic Workflows has no dynamic plugins");
    if matches!(access, ConfigAccess::Local(_)) {
        let client = held_user(home).await?;
        anyhow::ensure!(
            access.actor_did().as_deref() == Some(client.did()),
            "pack enable requires the held User identity selected by the local transaction"
        );
        let scope = PackApprovalScope {
            node_did: owner.to_owned(),
            pack_digests: vec![source.digest().to_owned()],
            nonce: uuid::Uuid::new_v4().to_string(),
        };
        let approver = Arc::new(LocalWireApprover {
            actor: client.client_signing_capability()?,
            scope: scope.clone(),
        });
        access
            .transact_write_verified_pack_with_finality(
                "operator.enable_dynamic_workflows",
                scope,
                approver,
                |txn| {
                    let jobs = &jobs;
                    Box::pin(enable_dynamic_in_txn(txn, owner, engineer, jobs))
                },
            )
            .await
            .map_err(|failure| {
                if failure.unconfirmed {
                    failure
                        .error
                        .context("enable commit was not confirmed; inspect policy before retrying")
                } else {
                    failure.error
                }
            })
    } else {
        access
            .transact_write("operator.enable_dynamic_workflows", |txn| {
                let jobs = &jobs;
                Box::pin(enable_dynamic_in_txn(txn, owner, engineer, jobs))
            })
            .await
    }
}

async fn enable_dynamic_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    owner: &str,
    engineer: &str,
    jobs: &[(String, Vec<String>)],
) -> Result<Value> {
    let (_, tools) =
        gents_documents::load_agent_context_and_tools_in_txn(txn, owner, engineer).await?;
    let mut tools = tools.context("Engineer has no Tools")?;
    let mut policies = Vec::new();
    for (coordinate, targets) in jobs {
        policies.push(crate::pack::operator_enable_dynamic(
            &mut tools, coordinate, targets,
        )?);
    }
    let plugins: Vec<_> = jobs
        .iter()
        .map(|(coordinate, _)| coordinate.clone())
        .collect();
    let enabled = !plugins.is_empty()
        && plugins.iter().all(|coordinate| {
            let job = tools
                .integrations
                .as_ref()
                .and_then(|i| i.plugins.as_ref())
                .and_then(|p| p.iter().find(|p| &p.plugin == coordinate))
                .and_then(|p| p.job.as_ref());
            crate::pack::dynamic_enabled(job)
        });
    anyhow::ensure!(enabled, "explicit enable did not establish run policy");
    gents_documents::write_tools_document_in_txn(
        txn,
        &tools,
        &gents_inference::resolved::ProviderProfilePolicy,
    )
    .await?;
    Ok(json!({"enabled":enabled, "policies":policies}))
}

/// Options for a wiring batch. Dynamic enablement is a separate operator
/// transition; neither preview nor apply can elevate stored dynamic policy.
#[derive(Debug, Clone, Copy, Default)]
pub struct WireBatchOptions {
    pub tool_calls_consent: bool,
    pub preview: bool,
    pub warning_surface: super::PolicyWarningSurface,
}

pub async fn wire_pack_batch(
    access: &ConfigAccess,
    owner: &str,
    home: &Path,
    pack: &ResolvedPack,
    agents: &[String],
    options: WireBatchOptions,
    held_user: LoadHeldUser,
) -> Result<Vec<Value>> {
    let WireBatchOptions {
        tool_calls_consent,
        preview,
        warning_surface,
    } = options;
    if agents.is_empty() || pack.manifest().metadata.plugins.is_empty() {
        return Ok(Vec::new());
    }
    if preview {
        let pending = crate::pack::pending_agent_slots(access, pack.manifest(), owner, &|path| {
            Ok(pack.asset(path)?.to_vec())
        })
        .await?;
        if !pending.is_empty() {
            return Ok(pending
                .into_iter()
                .map(|slot| {
                    json!({
                        "slot": slot, "preview": true, "skipped": true,
                        "reason": "needs runtime identity provisioning",
                        "next": "Install this pack, then wire it to the selected agent(s)."
                    })
                })
                .collect());
        }
    }
    if let ConfigAccess::Graphql(endpoint) = access {
        if access.actor_did().as_deref() == Some(owner) {
            let value = operator_remote(
                endpoint,
                owner,
                &PackOperatorAction::WireBatch {
                    digest: pack.digest().to_owned(),
                    agents: agents.to_vec(),
                    preview,
                    tool_calls_consent,
                },
            )
            .await?;
            let mut receipts: Vec<Value> = serde_json::from_value(value)?;
            for receipt in &mut receipts {
                super::plugin_wiring::render_wire_receipt(receipt, warning_surface)?;
            }
            return Ok(receipts);
        }
    }
    let namespace = &pack.manifest().metadata.namespace;
    let lock_home = home.to_owned();
    let _guard =
        tokio::task::spawn_blocking(move || crate::plugin::store::try_lock_store(&lock_home))
            .await??;
    let preview_config = if preview && pack.manifest().config.is_some() {
        Some(
            crate::pack::load_pack_recorded_config(
                access,
                pack.manifest(),
                &crate::pack::PackInstallOptions {
                    node_did: owner.to_owned(),
                },
                &|path| Ok(pack.asset(path)?.to_vec()),
                &|name| {
                    if name == "GENTS_PACK_NODE_DID" {
                        Some(owner.to_owned())
                    } else {
                        None
                    }
                },
            )
            .await?,
        )
    } else {
        None
    };
    let mut plans = Vec::new();
    for agent in agents {
        for shipped in &pack.manifest().metadata.plugins {
            let plugin = if preview {
                use sha2::{Digest, Sha256};
                let artifact = pack.asset(&shipped.artifact)?;
                crate::plugin::store::InstalledPlugin {
                    namespace: namespace.clone(),
                    name: shipped.name.clone(),
                    version: pack.manifest().version.clone(),
                    digest: format!("sha256:{:x}", Sha256::digest(artifact)),
                    language: shipped.language.clone(),
                    declaration: shipped.clone(),
                    granted: None,
                    instructions: shipped
                        .instructions
                        .as_ref()
                        .map(|path| {
                            pack.asset(path)
                                .and_then(|bytes| Ok(std::str::from_utf8(bytes)?.to_owned()))
                        })
                        .transpose()?,
                    owner_pack_coordinate: Some(format!("{namespace}/{}", pack.manifest().name)),
                    owner_pack_digest: Some(pack.digest().to_owned()),
                    model_binding: None,
                    data_unique_keys: Default::default(),
                    data_collection_fields: Default::default(),
                    data_collection_schemas: Default::default(),
                    plugin_interface: pack.manifest().plugin_interface,
                }
            } else {
                read_record(home, namespace, &shipped.name)?
            };
            let mut plan =
                crate::pack::wiring::PluginWiring::read(access, owner, home, agent, plugin, &[])
                    .await?;
            if preview {
                if let Some(config) = &preview_config {
                    plan.skill_ids = config
                        .skills
                        .iter()
                        .map(|skill| skill.skill_id.clone())
                        .collect();
                }
                let coordinate = format!("{namespace}/{}", shipped.name);
                plan.reviewed_override = Some(preview_config.as_ref().and_then(|config| {
                    config
                        .tools
                        .iter()
                        .flat_map(|t| {
                            t.integrations
                                .as_ref()
                                .and_then(|i| i.plugins.as_ref())
                                .into_iter()
                                .flatten()
                        })
                        .find(|p| p.plugin == coordinate && p.job.is_some())
                        .and_then(|p| p.job.clone())
                }));
            }
            plan.preview = preview;
            plan.tool_calls_consent = tool_calls_consent;
            plan.next_step = explain_tools_command(agent);
            plans.push(plan);
        }
    }
    let reports = if preview {
        access
            .transact_readonly("pack.wire_batch.preview", |txn| {
                let plans = &plans;
                let preview_config = preview_config.as_ref();
                Box::pin(async move {
                    crate::pack::wiring::preview_batch(
                        txn,
                        plans,
                        preview_config,
                        &gents_inference::resolved::ProviderProfilePolicy,
                    )
                    .await
                })
            })
            .await?
    } else if matches!(access, ConfigAccess::Local(_)) {
        let client = held_user(home).await?;
        anyhow::ensure!(
            access.actor_did().as_deref() == Some(client.did()),
            "pack wire requires the held User identity selected by the local transaction"
        );
        let scope = PackApprovalScope {
            node_did: owner.to_owned(),
            pack_digests: plans.iter().map(|p| p.plugin.digest.clone()).collect(),
            nonce: uuid::Uuid::new_v4().to_string(),
        };
        let approver = Arc::new(LocalWireApprover {
            actor: client.client_signing_capability()?,
            scope: scope.clone(),
        });
        access.transact_verified_pack_with_approver("pack.wire_batch", scope, approver, |txn| {
            let plans = &plans;
            Box::pin(async move { crate::pack::wiring::apply_batch(txn.approved_interpreter()?, plans, false,
                &gents_inference::resolved::ProviderProfilePolicy).await })
        }).await.map_err(|failure| if failure.unconfirmed {
            failure.error.context("batch wiring commit was not confirmed; inspect all targets before retrying")
        } else { failure.error })?
    } else {
        let caller = access.client_held_signer_did();
        anyhow::ensure!(
            caller.is_some() && access.actor_did().as_deref() == caller,
            "pack wire requires the caller identity selected by the hosted transaction"
        );
        // Hosted installs carry the caller's delegated signing endpoint, not
        // a local held User key. Keep the entire batch in that one caller-
        // authorized transaction, just as the single-plugin wire does.
        access.transact_write_with_finality("pack.wire_batch", |txn| {
            let plans = &plans;
            Box::pin(async move {
                crate::pack::wiring::apply_batch(txn, plans, false,
                    &gents_inference::resolved::ProviderProfilePolicy).await
            })
        }).await.map_err(|failure| if failure.unconfirmed {
            failure.error.context("batch wiring commit was not confirmed; inspect all targets before retrying")
        } else { failure.error })?
    };
    Ok(plans.iter().zip(reports).map(|(plan, mut report)| {
        for warning in &mut report.policy_warnings { warning.render_for(warning_surface); }
        json!({
        "plugin": format!("{}/{}", plan.plugin.namespace, plan.plugin.name),
        "agent": plan.agent, "preview": preview,
        "authority": describe_plugin_authority(&plan.plugin.declaration, &plan.plugin.ceiling()),
        "report": report,
    })}).collect())
}

/// What the plugins of `manifest` and its `dependencies` ask for beyond a
/// sealed sandbox, as `[{plugin, asks}]`: what an install grants only with
/// the installer's consent.
pub fn requested_authority(
    manifest: &crate::pack::PackManifest,
    dependencies: &[&crate::pack::PackManifest],
) -> Result<Value> {
    Ok(
        crate::pack::install_review::identity_and_authority(manifest, dependencies)?["authority"]
            .take(),
    )
}

/// Attaches the Skill documents the installed pack `coordinate` (archive
/// `digest`) ships to `agent`'s Context, through the transaction a wire
/// writes with. Returns the skills the pack ships.
pub async fn attach_pack_skills(
    access: &ConfigAccess,
    owner: &str,
    home: &Path,
    agent: &str,
    coordinate: &str,
    digest: &str,
    held_user: LoadHeldUser,
) -> Result<Vec<String>> {
    let skills: Vec<String> = installed_pack_documents(access, owner, coordinate)
        .await?
        .into_iter()
        .filter(|(collection, _)| *collection == Collection::Skill)
        .map(|(_, id)| id)
        .collect();
    if skills.is_empty() {
        return Ok(skills);
    }
    let plan = SkillPlan {
        owner,
        agent,
        skills: &skills,
    };
    if matches!(access, ConfigAccess::Local(_)) {
        let client = held_user(home).await?;
        anyhow::ensure!(
            access.actor_did().as_deref() == Some(client.did()),
            "attaching pack skills requires the held User identity selected by the local transaction"
        );
        let scope = PackApprovalScope {
            node_did: owner.to_owned(),
            pack_digests: vec![digest.to_owned()],
            nonce: uuid::Uuid::new_v4().to_string(),
        };
        let approver = Arc::new(LocalWireApprover {
            actor: client.client_signing_capability()?,
            scope: scope.clone(),
        });
        access
            .transact_verified_pack_with_approver(
                "cli.pack.attach_skills",
                scope,
                approver,
                |txn| {
                    let plan = &plan;
                    Box::pin(async move { plan.apply(txn.approved_interpreter()?).await })
                },
            )
            .await
            .map_err(|failure| failure.error)?;
    } else {
        access
            .transact_write("cli.pack.attach_skills", |txn| {
                let plan = &plan;
                Box::pin(async move { plan.apply(txn).await })
            })
            .await?;
    }
    Ok(skills)
}

struct SkillPlan<'a> {
    owner: &'a str,
    agent: &'a str,
    skills: &'a [String],
}

impl SkillPlan<'_> {
    async fn apply(&self, txn: &ConfigApplyTxn<'_>) -> Result<()> {
        let (context, _) = load_agent_context_and_tools_in_txn(txn, self.owner, self.agent).await?;
        let mut context =
            context.with_context(|| format!("{} has no Context document", self.agent))?;
        let added: Vec<String> = self
            .skills
            .iter()
            .filter(|id| !context.skill_ids.contains(id))
            .cloned()
            .collect();
        if added.is_empty() {
            return Ok(());
        }
        context.skill_ids.extend(added);
        let value = serde_json::to_value(&context)?;
        let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
            collection: Collection::AgentContext,
            add: value.clone(),
            update: value,
        }])?;
        apply_desired_state_plan(
            txn,
            &plan,
            &gents_inference::resolved::ProviderProfilePolicy,
        )
        .await?;
        Ok(())
    }
}

struct LocalWireApprover {
    actor: ClientSigningCapability,
    scope: PackApprovalScope,
}

#[async_trait::async_trait]
impl PackEffectApprover for LocalWireApprover {
    async fn approve_graph(
        &self,
        _txn: &ConfigApplyTxn<'_>,
        mutation: &PackMutation,
        history: Option<Value>,
    ) -> std::result::Result<PackEvidence, PackApprovalError> {
        gents_store::config_client::prepare_pack_evidence_from_history(
            &self.actor,
            &self.scope,
            mutation,
            history,
        )
        .await
        .map_err(|error| PackApprovalError::Refused(error.to_string()))
    }
    async fn approve_relationship(
        &self,
        _txn: &ConfigApplyTxn<'_>,
        relationship: &PackRelationship,
    ) -> std::result::Result<PackRelationshipEvidence, PackApprovalError> {
        gents_store::config_client::prepare_pack_relationship(
            &self.actor,
            &self.scope,
            relationship,
        )
        .await
        .map_err(|error| PackApprovalError::Refused(error.to_string()))
    }
    async fn next_timestamp(
        &self,
        _txn: &ConfigApplyTxn<'_>,
        _kind: &'static str,
    ) -> std::result::Result<String, PackApprovalError> {
        Ok(gents_store::config_client::mint_recreate_identity_timestamp())
    }
    async fn graph_applied(&self, _result: &Value) -> std::result::Result<(), PackApprovalError> {
        Ok(())
    }
    async fn relationship_applied(&self) -> std::result::Result<(), PackApprovalError> {
        Ok(())
    }
    async fn client_finished(
        &self,
        _txn: &ConfigApplyTxn<'_>,
    ) -> std::result::Result<(), PackApprovalError> {
        Ok(())
    }
}

/// The runtime route a served wire request is sent to.
pub const WIRE_PATH: &str = "/packs/wire";

/// A wire request as a runtime's operator route receives it.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireCommand {
    pub node_did: String,
    pub request: WireRequest,
}

/// Explicit operations admitted by the existing Node-signed operator route.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum PackOperatorAction {
    WireBatch {
        digest: String,
        agents: Vec<String>,
        preview: bool,
        tool_calls_consent: bool,
    },
    EnableDynamic {
        digest: String,
        agent: String,
    },
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackOperatorCommand {
    pub node_did: String,
    pub action: PackOperatorAction,
}

/// Backwards compatible with single-plugin wire requests.
#[derive(serde::Deserialize)]
#[serde(untagged)]
pub enum OperatorCommand {
    Single(WireCommand),
    Pack(PackOperatorCommand),
}

impl OperatorCommand {
    pub fn node_did(&self) -> &str {
        match self {
            Self::Single(command) => &command.node_did,
            Self::Pack(command) => &command.node_did,
        }
    }
}

pub async fn operator_remote(
    endpoint: &gents_store::config_client::GraphqlEndpoint,
    owner: &str,
    action: &PackOperatorAction,
) -> Result<Value> {
    send_operator(endpoint, &json!({"node_did": owner, "action": action})).await
}

/// Sends `request` to the runtime serving `endpoint`'s node, once: its
/// outcome is unconfirmed when no answer arrives, so it is never retried.
pub async fn wire_remote(
    endpoint: &gents_store::config_client::GraphqlEndpoint,
    owner: &str,
    request: &WireRequest,
) -> Result<Value> {
    send_operator(
        endpoint,
        &serde_json::to_value(WireCommand {
            node_did: owner.to_owned(),
            request: request.clone(),
        })?,
    )
    .await
}

async fn send_operator(
    endpoint: &gents_store::config_client::GraphqlEndpoint,
    command: &Value,
) -> Result<Value> {
    let mut url = reqwest::Url::parse(endpoint.url())?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "pack wiring requires an HTTP URL without credentials"
    );
    let prefix = url
        .path()
        .strip_suffix("/api/v0/graphql")
        .ok_or_else(|| anyhow::anyhow!("pack wiring requires a Gents runtime GraphQL endpoint"))?;
    url.set_path(&format!("{prefix}{WIRE_PATH}"));
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let response = endpoint
        .authorize(client.post(url))?
        .json(command)
        .send()
        .await
        .map_err(|error| {
            anyhow::anyhow!(
            "pack wiring outcome is unconfirmed; inspect the agent's Tools before retrying: {error}"
        )
        })?;
    let status = response.status();
    let bytes = response.bytes().await.map_err(|error| {
        anyhow::anyhow!(
        "pack wiring response was unreadable; inspect the agent's Tools before retrying: {error}"
    )
    })?;
    anyhow::ensure!(
        status.is_success(),
        "pack wiring returned HTTP {status}: {}",
        String::from_utf8_lossy(&bytes)
    );
    serde_json::from_slice(&bytes).map_err(|error| {
        anyhow::anyhow!(
        "pack wiring response was unreadable; inspect the agent's Tools before retrying: {error}"
    )
    })
}

#[cfg(test)]
mod remote_tests {
    use super::*;
    use axum::{routing::post, Json, Router};
    use std::sync::Mutex;

    #[test]
    fn wire_targets_follow_the_lean_model() {
        gents_lean_vocab::families::contracts_pack_installation_wire_target::replay(
            |input: &gents_lean_vocab::families::contracts_pack_installation_wire_target::WireTargetInput| {
                wire_target_allowed(input.local, input.preview, input.remove)
            },
        );
    }

    fn request() -> WireRequest {
        WireRequest {
            plugin: "acme/tool".into(),
            agent: "assistant".into(),
            targets_from: vec!["acme/pack".into()],
            remove: false,
            preview: false,
            tool_calls_consent: false,
        }
    }

    #[tokio::test]
    async fn remote_wire_sends_one_bounded_request_to_runtime() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new().route(
            WIRE_PATH,
            post({
                let received = received.clone();
                move |Json(command): Json<WireCommand>| {
                    let received = received.clone();
                    async move {
                        received.lock().unwrap().push(command);
                        Json(serde_json::json!({"wired": true}))
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let endpoint = gents_store::config_client::GraphqlEndpoint::anonymous(format!(
            "http://{address}/api/v0/graphql"
        ));
        let result = wire_remote(&endpoint, "did:key:owner", &request())
            .await
            .unwrap();
        assert_eq!(result["wired"], true);
        let received = received.lock().unwrap();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].node_did, "did:key:owner");
        assert_eq!(received[0].request.plugin, "acme/tool");
        assert_eq!(received[0].request.targets_from, ["acme/pack"]);
        server.abort();
    }

    #[tokio::test]
    async fn lost_response_is_unconfirmed_and_is_not_retried() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            drop(stream);
        });
        let endpoint = gents_store::config_client::GraphqlEndpoint::anonymous(format!(
            "http://{address}/api/v0/graphql"
        ));
        let error = wire_remote(&endpoint, "did:key:owner", &request())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("outcome is unconfirmed"));
        server.await.unwrap();
    }
}
