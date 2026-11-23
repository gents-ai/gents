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

/// Wires a new pin through the runtime serving `endpoint`'s node, the client
/// signing each of its writes with the User key it holds
/// (`Identity.UserDelegation.runtimeSignsWithHeldUserKey`), from the plan
/// it made from a pack it chose (`Configuration.wirePlanSubject`); `None`
/// plans from the client's own home, which the runtime serves.
pub type WireAsClient = fn(
    gents_store::config_client::GraphqlEndpoint,
    String,
    std::path::PathBuf,
    WireRequest,
    Option<ClientPlan>,
) -> futures::future::BoxFuture<'static, Result<Value>>;

/// What a client plans a wire over a runtime from when the runtime serves
/// another home (`Configuration.wirePlanSubject`): the plugin's record and
/// the approvals that vouch for the packs the wire reads.
#[derive(Debug, Clone)]
pub struct ClientPlan {
    pub record: crate::plugin::store::InstalledPlugin,
    /// `None` plans from a pack the client just sent, whose documents the
    /// client vouches for itself.
    pub approvals: Option<Vec<crate::pack::approvals::PackApproval>>,
}

struct Approved<'a> {
    approvals: Option<&'a [crate::pack::approvals::PackApproval]>,
    record_digest: Option<&'a str>,
    agent: &'a str,
}

const APPROVE_NEXT: &str = "Next: gents config pack approve <pack source>";

impl crate::pack::wiring::Vouch for Approved<'_> {
    fn own(&self, pack: &str, collection: Collection, ids: &[String]) -> Result<()> {
        use crate::pack::approvals::{own_approval, own_pack_ids};
        let Some(approvals) = self.approvals else {
            return Ok(());
        };
        let digest = self.record_digest.unwrap_or_default();
        let approval = own_approval(approvals, digest).with_context(|| {
            format!("{pack} at {digest} is not a pack this device approved, so nothing of it is wired. {APPROVE_NEXT}")
        })?;
        own_pack_ids(approvals, digest, self.agent, collection, ids)
            .map(drop)
            .with_context(|| match approval.yields {
                Some(_) => format!("the runtime reports {collection:?} documents of {pack} that the version this device approved does not yield for {}, so nothing is wired", self.agent),
                None => format!("this device could not derive the documents of {pack} when it approved it, so nothing of it is wired. {APPROVE_NEXT} with the install variables the pack reads"),
            })
    }

    fn target(
        &self,
        pack: &str,
        installed: Option<&str>,
        ids: &[String],
    ) -> Result<crate::pack::InstalledVia> {
        use crate::pack::approvals::{target_approval, target_pack_ids};
        let Some(approvals) = self.approvals else {
            return Ok(crate::pack::InstalledVia::Operator);
        };
        let Some(approval) = target_approval(approvals, pack, installed) else {
            let versions = approvals
                .iter()
                .filter(|approval| approval.coordinate == pack)
                .count();
            anyhow::bail!(match versions {
                0 => format!("{pack} is not a pack this device approved, so nothing of it is wired. {APPROVE_NEXT}"),
                _ => format!("the node records {pack} at a version this device did not approve, so which approval vouches for it is unknown and nothing of it is wired. {APPROVE_NEXT}"),
            });
        };
        target_pack_ids(approvals, pack, installed, ids)
            .with_context(|| match approval.yields {
                Some(_) => format!("the runtime reports AgentTarget documents of {pack} that the version this device approved does not yield, so nothing is wired"),
                None => format!("this device could not derive the documents of {pack} when it approved it, so nothing of it is wired. {APPROVE_NEXT} with the install variables and agent bindings the pack reads"),
            })?;
        Ok(approval.provenance())
    }
}

/// Who signs a wire: the held User of a home a local transaction opens, or
/// the client itself through the runtime serving the node.
#[derive(Clone, Copy)]
pub struct WireSigners {
    pub held_user: LoadHeldUser,
    pub client: WireAsClient,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
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
/// A preview computes the same report and writes nothing. A new pin over
/// GraphQL is written in the runtime's transaction, signed by the client
/// (`signers.client`).
pub async fn wire_in(
    access: &ConfigAccess,
    owner: &str,
    home: &Path,
    request: &WireRequest,
    signers: WireSigners,
) -> Result<Value> {
    wire_in_planned(access, owner, home, request, None, signers).await
}

/// [`wire_in`] with the plan a client made from a pack it sent or approved
/// (`Configuration.wirePlanSubject`), for a new pin written through a
/// runtime that serves another home.
pub async fn wire_in_planned(
    access: &ConfigAccess,
    owner: &str,
    home: &Path,
    request: &WireRequest,
    record: Option<ClientPlan>,
    signers: WireSigners,
) -> Result<Value> {
    if let ConfigAccess::Graphql(endpoint) = access {
        if !request.preview && !request.remove {
            return (signers.client)(
                endpoint.clone(),
                owner.to_owned(),
                home.to_owned(),
                request.clone(),
                record,
            )
            .await;
        }
    }
    wire_in_spawned(access, owner, home, request, signers.held_user).await
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
    let WirePlan {
        _store_guard,
        plugin,
        plan,
    } = WirePlan::read(
        access,
        owner,
        &crate::plugin::store::StoreWriter::Offline(home.to_owned()),
        request,
    )
    .await?;
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

    wire_outcome(request, &plugin, &report)
}

/// What a wire reports to its caller.
pub fn wire_outcome(
    request: &WireRequest,
    plugin: &crate::plugin::store::InstalledPlugin,
    report: &crate::pack::WireReport,
) -> Result<Value> {
    Ok(json!({
        "plugin": request.plugin,
        "agent": request.agent,
        "preview": request.preview,
        "authority": describe_plugin_authority(&plugin.declaration, &plugin.ceiling()),
        "report": serde_json::to_value(report)?,
    }))
}

/// A wire's plan, read as `access`'s actor, holding its writer's plugin
/// store lock until it is applied.
pub struct WirePlan {
    _store_guard: Option<crate::plugin::store::StoreGuard>,
    pub plugin: crate::plugin::store::InstalledPlugin,
    pub plan: crate::pack::wiring::PluginWiring,
}

impl WirePlan {
    pub async fn read(
        access: &ConfigAccess,
        owner: &str,
        writer: &crate::plugin::store::StoreWriter,
        request: &WireRequest,
    ) -> Result<Self> {
        Self::read_holding(access, owner, writer.home(), request, Some(writer.clone())).await
    }

    /// The plan for `plugin`, a record the client planned from a pack it
    /// holds (`Configuration.wirePlanSubject`), without reading or holding
    /// any plugin store: what a client signs against while the runtime
    /// that serves another home holds its own.
    pub async fn of_plan(
        access: &ConfigAccess,
        owner: &str,
        request: &WireRequest,
        client: ClientPlan,
    ) -> Result<Self> {
        let coordinate = format!("{}/{}", client.record.namespace, client.record.name);
        let record_digest = client.record.owner_pack_digest.as_deref();
        let reviewed = client.approvals.as_deref().map(|approvals| {
            crate::pack::wiring::ReviewedJob::Approved(
                crate::pack::approvals::own_approval(approvals, record_digest.unwrap_or_default())
                    .and_then(|approval| approval.yields.as_ref())
                    .and_then(|yields| yields.job(&coordinate, &request.agent)),
            )
        });
        let mut plan = crate::pack::wiring::PluginWiring::read_with(
            access,
            owner,
            &request.agent,
            client.record.clone(),
            &request.targets_from,
            &Approved {
                approvals: client.approvals.as_deref(),
                record_digest,
                agent: &request.agent,
            },
        )
        .await?;
        plan.reviewed = reviewed.unwrap_or_default();
        Ok(Self::finish(None, client.record, plan, request))
    }

    /// [`Self::read`] without holding the plugin store: what a client
    /// reads beside the runtime that holds it while it writes.
    pub async fn read_unheld(
        access: &ConfigAccess,
        owner: &str,
        home: &Path,
        request: &WireRequest,
    ) -> Result<Self> {
        Self::read_holding(access, owner, home, request, None).await
    }

    async fn read_holding(
        access: &ConfigAccess,
        owner: &str,
        home: &Path,
        request: &WireRequest,
        holder: Option<crate::plugin::store::StoreWriter>,
    ) -> Result<Self> {
        let home_for_read = home.to_owned();
        let plugin_name = request.plugin.clone();
        let guard = match &holder {
            Some(writer) => Some(
                writer
                    .lock_until(tokio::time::Instant::now() + crate::plugin::store::LEASE_WAIT)
                    .await?,
            ),
            None => None,
        };
        let (store_guard, plugin) = tokio::task::spawn_blocking(move || {
            let (namespace, name) = split_pack_coordinate(&plugin_name);
            let plugin = read_record(&home_for_read, namespace, name).with_context(|| {
                format!("{plugin_name} is not installed. Next: gents config pack install <pack>")
            })?;
            let plugin = crate::pack::wire_subject::wire_plan_record(
                &plugin_name,
                &[],
                crate::pack::wire_subject::WireRecord::OwnHome(plugin),
            )
            .with_context(|| format!("the plugin store records another plugin as {plugin_name}"))?;
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
        Self::planned(access, owner, home, request, store_guard, plugin).await
    }

    async fn planned(
        access: &ConfigAccess,
        owner: &str,
        home: &Path,
        request: &WireRequest,
        store_guard: Option<crate::plugin::store::StoreGuard>,
        plugin: crate::plugin::store::InstalledPlugin,
    ) -> Result<Self> {
        let plan = crate::pack::wiring::PluginWiring::read(
            access,
            owner,
            home,
            &request.agent,
            plugin.clone(),
            &request.targets_from,
        )
        .await?;
        Ok(Self::finish(store_guard, plugin, plan, request))
    }

    fn finish(
        store_guard: Option<crate::plugin::store::StoreGuard>,
        plugin: crate::plugin::store::InstalledPlugin,
        mut plan: crate::pack::wiring::PluginWiring,
        request: &WireRequest,
    ) -> Self {
        plan.remove = request.remove;
        plan.preview = request.preview;
        plan.dynamic_consent = request
            .dynamic_consent
            .then_some(crate::pack::consent_dynamic_run as _);
        plan.tool_calls_consent = request.tool_calls_consent;
        plan.next_step = explain_tools_command(&request.agent);
        Self {
            _store_guard: store_guard,
            plugin,
            plan,
        }
    }
}

/// Wires each plugin `pack` shipped into each of `agents`, agent by agent,
/// through the same writer as `gents config pack wire`.
pub async fn wire_pack_plugins(
    access: &ConfigAccess,
    owner: &str,
    home: &Path,
    pack: &ResolvedPack,
    agents: &[String],
    approval: Option<&crate::pack::approvals::PackApproval>,
    signers: WireSigners,
) -> Result<Vec<Value>> {
    wire_pack_plugins_consenting(
        access,
        owner,
        home,
        SentPack {
            archive: pack,
            approval,
        },
        agents,
        WireConsent {
            dynamic: false,
            tool_calls: false,
        },
        signers,
    )
    .await
}

/// What the node owner approved for a pack's plugins when its install was
/// approved: dynamic scripts in their job blocks, and calls to each agent's
/// own tools ([`WireRequest`]'s `dynamic_consent` and `tool_calls_consent`).
#[derive(Clone, Copy)]
pub struct WireConsent {
    pub dynamic: bool,
    pub tool_calls: bool,
}

/// A pack the client sent, with its approval of the pack: with one, a wire
/// over a runtime that serves another home signs only documents that
/// approval vouches for.
#[derive(Clone, Copy)]
pub struct SentPack<'a> {
    pub archive: &'a ResolvedPack,
    pub approval: Option<&'a crate::pack::approvals::PackApproval>,
}

/// [`wire_pack_plugins`], with the node owner's `consent`.
pub async fn wire_pack_plugins_consenting(
    access: &ConfigAccess,
    owner: &str,
    home: &Path,
    pack: SentPack<'_>,
    agents: &[String],
    consent: WireConsent,
    signers: WireSigners,
) -> Result<Vec<Value>> {
    let WireConsent {
        dynamic: dynamic_consent,
        tool_calls: tool_calls_consent,
    } = consent;
    let sent = crate::pack::wire_subject::WireSubject::of_sent_pack(pack.archive)?;
    let approvals = pack.approval.map(|approval| vec![approval.clone()]);
    let mut wired = Vec::new();
    for agent in agents {
        for subject in &sent {
            let request = WireRequest {
                plugin: subject.coordinate(),
                agent: agent.clone(),
                targets_from: Vec::new(),
                remove: false,
                preview: false,
                dynamic_consent,
                tool_calls_consent,
            };
            let plan = crate::pack::wire_subject::wire_plan_record(
                &request.plugin,
                &[],
                crate::pack::wire_subject::WireRecord::SentPack(subject.clone()),
            )
            .map(|record| ClientPlan {
                record,
                approvals: approvals.clone(),
            });
            wired.push(wire_in_planned(access, owner, home, &request, plan, signers).await?);
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
        txn: &ConfigApplyTxn<'_>,
        mutation: &PackMutation,
        history: Option<Value>,
    ) -> std::result::Result<PackEvidence, PackApprovalError> {
        let ids = txn
            .home_policy_ids()
            .await
            .map_err(|error| PackApprovalError::Refused(error.to_string()))?;
        gents_store::config_client::prepare_pack_evidence_from_history(
            &self.actor,
            &self.scope,
            mutation,
            history,
            ids,
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

/// The runtime route that serves an installed plugin's record to a client
/// wiring it (`Configuration.wirePlanSubject`).
pub const PLUGIN_RECORD_PATH: &str = "/packs/plugins/record";

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireCommand {
    pub node_did: String,
    pub request: WireRequest,
}

#[cfg(test)]
mod remote_tests {
    use super::*;

    #[test]
    fn wire_targets_follow_the_lean_model() {
        gents_lean_vocab::families::contracts_pack_installation_wire_target::replay(
            |input: &gents_lean_vocab::families::contracts_pack_installation_wire_target::WireTargetInput| {
                wire_target_allowed(input.local, input.preview, input.remove)
            },
        );
    }
}
