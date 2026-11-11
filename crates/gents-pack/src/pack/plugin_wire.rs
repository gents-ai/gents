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

/// Loads the held User identity a write through a local transaction signs
/// with. A preview, or a write over GraphQL, never calls it, so it never
/// loads (or creates) that identity for them.
pub type LoadHeldUser = fn() -> futures::future::BoxFuture<
    'static,
    Result<Arc<dyn gents_store::identity::NodeIdentity>>,
>;

#[derive(Clone)]
pub struct WireRequest {
    pub plugin: String,
    pub agent: String,
    pub targets_from: Vec<String>,
    pub remove: bool,
    pub preview: bool,
    /// The node owner approved dynamic scripts for this plugin (the init
    /// approval of its pack): wiring raises its job block from `check` to
    /// `run` (Lean `consentedDynamic`). The wire command never sets it.
    pub dynamic_consent: bool,
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
/// A preview computes the same report and writes nothing.
pub async fn wire_in(
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

async fn wire_in_owned(
    access: &ConfigAccess,
    owner: &str,
    home: &Path,
    request: &WireRequest,
    held_user: LoadHeldUser,
) -> Result<Value> {
    if !request.preview && !request.remove {
        anyhow::ensure!(matches!(access, ConfigAccess::Local(_)),
            "cannot publish a plugin pin through GraphQL because the commit outcome may be unknown; stop the runtime serving the owning node home, then run `gents config pack wire {} --agent {} --home <node-home>` without --graphql",
            request.plugin, request.agent);
    }
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
    plan.dynamic_consent = request
        .dynamic_consent
        .then_some(crate::pack::consent_dynamic_run as _);
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
        let client = held_user().await?;
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
    wire_pack_plugins_consenting(access, owner, home, pack, agents, false, false, held_user).await
}

/// [`wire_pack_plugins`], with the node owner's consent to dynamic scripts
/// for this pack's job blocks when `dynamic_consent` is set, and to its
/// plugins calling each agent's own tools when `tool_calls_consent` is.
#[expect(
    clippy::too_many_arguments,
    reason = "each consent is a separate flag the node owner approves"
)]
pub async fn wire_pack_plugins_consenting(
    access: &ConfigAccess,
    owner: &str,
    home: &Path,
    pack: &ResolvedPack,
    agents: &[String],
    dynamic_consent: bool,
    tool_calls_consent: bool,
    held_user: LoadHeldUser,
) -> Result<Vec<Value>> {
    let namespace = &pack.manifest().metadata.namespace;
    let mut wired = Vec::new();
    for agent in agents {
        for plugin in &pack.manifest().metadata.plugins {
            let request = WireRequest {
                plugin: format!("{namespace}/{}", plugin.name),
                agent: agent.clone(),
                targets_from: Vec::new(),
                remove: false,
                preview: false,
                dynamic_consent,
                tool_calls_consent,
            };
            wired.push(wire_in(access, owner, home, &request, held_user).await?);
        }
    }
    Ok(wired)
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
        let client = held_user().await?;
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
