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
use crate::pack::WireReport;
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

/// Wires new pins through the runtime serving `endpoint`'s node in one
/// transaction, the client signing each of its writes with the User key it
/// holds (`Identity.UserDelegation.runtimeSignsWithHeldUserKey`), from the
/// plans it made (`Configuration.wirePlanSubject`).
pub type WireAsClient = fn(
    gents_store::config_client::GraphqlEndpoint,
    String,
    std::path::PathBuf,
    Vec<ClientWire>,
) -> futures::future::BoxFuture<'static, Result<Vec<Value>>>;

/// One wire a client asks for, with the plan it made from a pack it chose;
/// `None` plans from the client's own home, which the runtime serves.
#[derive(Clone)]
pub struct ClientWire {
    pub request: WireRequest,
    pub plan: Option<ClientPlan>,
}

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
    plan: Option<ClientPlan>,
    signers: WireSigners,
) -> Result<Value> {
    let wire = ClientWire {
        request: request.clone(),
        plan,
    };
    single(wire_batch_in(access, owner, home, vec![wire], signers).await?)
}

fn single(mut outcomes: Vec<Value>) -> Result<Value> {
    anyhow::ensure!(
        outcomes.len() == 1,
        "one wire reported {} outcomes",
        outcomes.len()
    );
    Ok(outcomes.remove(0))
}

/// A batch that does not preview, remove or wire every pair alike.
#[derive(Debug, thiserror::Error)]
#[error("one wire batch previews, removes or wires every pair alike; this one mixes them")]
pub struct MixedWireBatch;

/// What every request of a batch does: (`preview`, `remove`).
fn batch_mode<'a>(mut requests: impl Iterator<Item = &'a WireRequest>) -> Result<(bool, bool)> {
    let Some(first) = requests.next() else {
        return Ok((false, false));
    };
    let mode = (first.preview, first.remove);
    if requests.any(|request| (request.preview, request.remove) != mode) {
        return Err(MixedWireBatch.into());
    }
    Ok(mode)
}

/// Every wire of `wires` in one transaction: all of them are written, or
/// none. A preview validates the whole write and makes none. New pins over
/// GraphQL are written in the runtime's transaction, signed by the client.
pub async fn wire_batch_in(
    access: &ConfigAccess,
    owner: &str,
    home: &Path,
    wires: Vec<ClientWire>,
    signers: WireSigners,
) -> Result<Vec<Value>> {
    if wires.is_empty() {
        return Ok(Vec::new());
    }
    let (preview, remove) = batch_mode(wires.iter().map(|wire| &wire.request))?;
    if let ConfigAccess::Graphql(endpoint) = access {
        if !preview && !remove {
            return (signers.client)(endpoint.clone(), owner.to_owned(), home.to_owned(), wires)
                .await;
        }
    }
    let requests: Vec<WireRequest> = wires.into_iter().map(|wire| wire.request).collect();
    wire_batch_spawned(access, owner, home, requests, signers.held_user).await
}

/// [`wire_in`] over `access` itself, never sent to a runtime: what refuses a
/// new pin over GraphQL.
pub async fn wire_in_spawned(
    access: &ConfigAccess,
    owner: &str,
    home: &Path,
    request: &WireRequest,
    held_user: LoadHeldUser,
) -> Result<Value> {
    single(wire_batch_spawned(access, owner, home, vec![request.clone()], held_user).await?)
}

async fn wire_batch_spawned(
    access: &ConfigAccess,
    owner: &str,
    home: &Path,
    requests: Vec<WireRequest>,
    held_user: LoadHeldUser,
) -> Result<Vec<Value>> {
    let access = access.clone();
    let owner = owner.to_owned();
    let home = home.to_owned();
    tokio::spawn(
        async move { wire_batch_owned(&access, &owner, &home, &requests, held_user).await },
    )
    .await?
}

pub fn wire_target_allowed(local: bool, preview: bool, remove: bool) -> bool {
    local || preview || remove
}

async fn wire_batch_owned(
    access: &ConfigAccess,
    owner: &str,
    home: &Path,
    requests: &[WireRequest],
    held_user: LoadHeldUser,
) -> Result<Vec<Value>> {
    let (preview, remove) = batch_mode(requests.iter())?;
    if let Some(request) = requests.first() {
        anyhow::ensure!(wire_target_allowed(matches!(access, ConfigAccess::Local(_)), preview, remove),
            "cannot publish a plugin pin through GraphQL because the commit outcome may be unknown; stop the runtime serving the owning node home, then run `gents config pack wire {} --agent {} --home <node-home>` without --graphql",
            request.plugin, request.agent);
    }
    let plan = WirePlan::read(access, owner, home, requests).await?;
    let reports = if preview {
        access
            .transact_readonly("cli.pack.wire.preview", |txn| {
                let plan = &plan;
                Box::pin(async move { plan.apply(txn, true).await })
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
            pack_digests: plan.digests(),
            nonce: uuid::Uuid::new_v4().to_string(),
        };
        let approver = Arc::new(LocalWireApprover {
            actor: client.client_signing_capability()?,
            scope: scope.clone(),
        });
        access
            .transact_verified_pack_with_approver("cli.pack.wire", scope, approver, |txn| {
                let plan = &plan;
                Box::pin(async move { plan.apply(txn.approved_interpreter()?, false).await })
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
                Box::pin(async move { plan.apply(txn, false).await })
            })
            .await?
    };
    plan.outcomes(&reports)
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

/// One wire of a [`WirePlan`].
pub struct PlannedWire {
    pub request: WireRequest,
    pub plugin: crate::plugin::store::InstalledPlugin,
    pub plan: crate::pack::wiring::PluginWiring,
}

/// A batch of wires' plans, read as `access`'s actor, with the plugin store
/// of `home` held until they are applied.
pub struct WirePlan {
    _store_guard: Option<crate::plugin::store::StoreGuard>,
    pub wires: Vec<PlannedWire>,
}

impl WirePlan {
    pub async fn read(
        access: &ConfigAccess,
        owner: &str,
        home: &Path,
        requests: &[WireRequest],
    ) -> Result<Self> {
        Self::read_holding(access, owner, home, requests, true).await
    }

    /// The plans a client makes beside the runtime that holds the plugin
    /// store while it writes: from the record of each plan the client made
    /// from a pack it holds (`Configuration.wirePlanSubject`), and otherwise
    /// from `home`, without holding its store.
    pub async fn for_client(
        access: &ConfigAccess,
        owner: &str,
        home: &Path,
        wires: Vec<ClientWire>,
    ) -> Result<Self> {
        let mut planned = Vec::with_capacity(wires.len());
        for ClientWire { request, plan } in wires {
            planned.push(match plan {
                Some(client) => Self::of_plan(access, owner, request, client).await?,
                None => {
                    let mut read = Self::read_holding(
                        access,
                        owner,
                        home,
                        std::slice::from_ref(&request),
                        false,
                    )
                    .await?;
                    read.wires.remove(0)
                }
            });
        }
        Ok(Self {
            _store_guard: None,
            wires: planned,
        })
    }

    async fn of_plan(
        access: &ConfigAccess,
        owner: &str,
        request: WireRequest,
        client: ClientPlan,
    ) -> Result<PlannedWire> {
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
        Ok(Self::finish(client.record, plan, request))
    }

    async fn read_holding(
        access: &ConfigAccess,
        owner: &str,
        home: &Path,
        requests: &[WireRequest],
        hold: bool,
    ) -> Result<Self> {
        let home_for_read = home.to_owned();
        let names: Vec<String> = requests.iter().map(|request| request.plugin.clone()).collect();
        let (store_guard, plugins) = tokio::task::spawn_blocking(move || {
            let guard = hold
                .then(|| crate::plugin::store::try_lock_store(&home_for_read))
                .transpose()?;
            let plugins = names
                .iter()
                .map(|plugin_name| {
                    let (namespace, name) = split_pack_coordinate(plugin_name);
                    let plugin = read_record(&home_for_read, namespace, name).with_context(|| {
                        format!("{plugin_name} is not installed. Next: gents config pack install <pack>")
                    })?;
                    crate::pack::wire_subject::wire_plan_record(
                        plugin_name,
                        &[],
                        crate::pack::wire_subject::WireRecord::OwnHome(plugin),
                    )
                    .with_context(|| {
                        format!("the plugin store records another plugin as {plugin_name}")
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok::<_, anyhow::Error>((guard, plugins))
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
        let mut wires = Vec::with_capacity(requests.len());
        for (request, plugin) in requests.iter().zip(plugins) {
            let plan = crate::pack::wiring::PluginWiring::read(
                access,
                owner,
                home,
                &request.agent,
                plugin.clone(),
                &request.targets_from,
            )
            .await?;
            wires.push(Self::finish(plugin, plan, request.clone()));
        }
        Ok(Self {
            _store_guard: store_guard,
            wires,
        })
    }

    fn finish(
        plugin: crate::plugin::store::InstalledPlugin,
        mut plan: crate::pack::wiring::PluginWiring,
        request: WireRequest,
    ) -> PlannedWire {
        plan.remove = request.remove;
        plan.tool_calls_consent = request.tool_calls_consent;
        plan.next_step = explain_tools_command(&request.agent);
        PlannedWire {
            request,
            plugin,
            plan,
        }
    }

    /// The installed artifacts the batch pins, each once.
    pub fn digests(&self) -> Vec<String> {
        let mut digests: Vec<String> = Vec::new();
        for wire in &self.wires {
            if !digests.contains(&wire.plugin.digest) {
                digests.push(wire.plugin.digest.clone());
            }
        }
        digests
    }

    /// Applies every wire in `txn` in one write; `preview` writes nothing.
    pub async fn apply(&self, txn: &ConfigApplyTxn<'_>, preview: bool) -> Result<Vec<WireReport>> {
        let plans: Vec<&crate::pack::wiring::PluginWiring> =
            self.wires.iter().map(|wire| &wire.plan).collect();
        crate::pack::wiring::apply_batch(
            txn,
            &plans,
            preview,
            &gents_inference::resolved::ProviderProfilePolicy,
        )
        .await
    }

    /// What each wire reports to its caller.
    pub fn outcomes(&self, reports: &[WireReport]) -> Result<Vec<Value>> {
        self.wires
            .iter()
            .zip(reports)
            .map(|(wire, report)| wire_outcome(&wire.request, &wire.plugin, report))
            .collect()
    }
}

/// What a wiring batch is given besides its pack and agents: the node
/// owner's consent, when its install was approved, to calls to each agent's
/// own tools. No wiring raises a dynamic policy
/// (Lean `PluginWiring.wire_never_enables`).
#[derive(Debug, Clone, Copy, Default)]
pub struct WireBatchOptions {
    pub tool_calls_consent: bool,
}

/// A pack the client sent, with its approval of the pack: with one, a wire
/// over a runtime that serves another home signs only documents that
/// approval vouches for.
#[derive(Clone, Copy)]
pub struct SentPack<'a> {
    pub archive: &'a ResolvedPack,
    pub approval: Option<&'a crate::pack::approvals::PackApproval>,
}

/// Wires each plugin `pack` shipped into each of `agents` in one
/// transaction: every pair is written, or none.
pub async fn wire_pack_batch(
    access: &ConfigAccess,
    owner: &str,
    home: &Path,
    pack: SentPack<'_>,
    agents: &[String],
    options: WireBatchOptions,
    signers: WireSigners,
) -> Result<Vec<Value>> {
    let sent = crate::pack::wire_subject::WireSubject::of_sent_pack(pack.archive)?;
    let approvals = pack.approval.map(|approval| vec![approval.clone()]);
    let mut wires = Vec::with_capacity(agents.len() * sent.len());
    for agent in agents {
        for subject in &sent {
            let request = WireRequest {
                plugin: subject.coordinate(),
                agent: agent.clone(),
                targets_from: Vec::new(),
                remove: false,
                preview: false,
                tool_calls_consent: options.tool_calls_consent,
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
            wires.push(ClientWire { request, plan });
        }
    }
    wire_batch_in(access, owner, home, wires, signers).await
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
    write_as_operator(
        access,
        owner,
        home,
        digest,
        "cli.pack.attach_skills",
        held_user,
        &plan,
    )
    .await?;
    Ok(skills)
}

/// The operator's enable of the dynamic scripts of each job plugin the
/// installed pack `coordinate` (archive `digest`) ships, on `agent`'s Tools,
/// over the dynamic targets the pack's own block names (Lean
/// `PluginWiring.operatorEnable`): the one way a block reaches `run`.
pub async fn enable_pack_dynamic(
    access: &ConfigAccess,
    owner: &str,
    home: &Path,
    agent: &str,
    coordinate: &str,
    digest: &str,
    held_user: LoadHeldUser,
) -> Result<Vec<crate::pack::DynamicEnableReceipt>> {
    let record = crate::pack::read_home_install(home, coordinate)?
        .with_context(|| format!("{coordinate} is not installed on this home"))?;
    let (namespace, _) = split_pack_coordinate(coordinate);
    let mut plans = Vec::new();
    for shipped in &record.plugins {
        let plugin = read_record(home, namespace, &shipped.name)?;
        if plugin.declaration.job.0 {
            plans.push(
                crate::pack::wiring::PluginWiring::read(access, owner, home, agent, plugin, &[])
                    .await?,
            );
        }
    }
    anyhow::ensure!(
        !plans.is_empty(),
        "{coordinate} ships no plugin that runs jobs, so it has no dynamic scripts to enable"
    );
    let plan = DynamicEnablePlan {
        owner,
        agent,
        plans: &plans,
    };
    write_as_operator(
        access,
        owner,
        home,
        digest,
        "cli.pack.enable_dynamic",
        held_user,
        &plan,
    )
    .await
}

/// One write a node owner makes on a pack's behalf.
#[async_trait::async_trait]
trait OperatorWrite: Sync {
    type Output: Send;
    async fn apply(&self, txn: &ConfigApplyTxn<'_>) -> Result<Self::Output>;
}

/// `plan` in one transaction: signed by the held User of a home a local
/// transaction opens, with the pack archive `digest` in its approval scope,
/// and otherwise as `access`'s actor.
async fn write_as_operator<P>(
    access: &ConfigAccess,
    owner: &str,
    home: &Path,
    digest: &str,
    operation: &'static str,
    held_user: LoadHeldUser,
    plan: &P,
) -> Result<P::Output>
where
    P: OperatorWrite,
{
    if matches!(access, ConfigAccess::Local(_)) {
        let client = held_user(home).await?;
        anyhow::ensure!(
            access.actor_did().as_deref() == Some(client.did()),
            "{operation} requires the held User identity selected by the local transaction"
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
            .transact_verified_pack_with_approver(operation, scope, approver, |txn| {
                Box::pin(async move { plan.apply(txn.approved_interpreter()?).await })
            })
            .await
            .map_err(|failure| failure.error)
    } else {
        access
            .transact_write(operation, |txn| Box::pin(plan.apply(txn)))
            .await
    }
}

struct DynamicEnablePlan<'a> {
    owner: &'a str,
    agent: &'a str,
    plans: &'a [crate::pack::wiring::PluginWiring],
}

#[async_trait::async_trait]
impl OperatorWrite for DynamicEnablePlan<'_> {
    type Output = Vec<crate::pack::DynamicEnableReceipt>;

    async fn apply(&self, txn: &ConfigApplyTxn<'_>) -> Result<Self::Output> {
        let (_, tools) = load_agent_context_and_tools_in_txn(txn, self.owner, self.agent).await?;
        let mut tools = tools.with_context(|| format!("{} has no Tools document", self.agent))?;
        let mut receipts = Vec::with_capacity(self.plans.len());
        for plan in self.plans {
            let coordinate = format!("{}/{}", plan.plugin.namespace, plan.plugin.name);
            let shipped = plan.reviewed_job(txn).await?.with_context(|| {
                format!("the pack of {coordinate} ships no job block, so it names no dynamic targets to enable")
            })?;
            let (enabled, receipt) = crate::pack::operator_enable_dynamic(
                &tools,
                &coordinate,
                &shipped.policy.dynamic_targets,
            )?;
            tools = enabled;
            receipts.push(receipt);
        }
        tools.validate()?;
        let value = serde_json::to_value(&tools)?;
        let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
            collection: Collection::Tools,
            add: value.clone(),
            update: value,
        }])?;
        apply_desired_state_plan(
            txn,
            &plan,
            &gents_inference::resolved::ProviderProfilePolicy,
        )
        .await?;
        Ok(receipts)
    }
}

struct SkillPlan<'a> {
    owner: &'a str,
    agent: &'a str,
    skills: &'a [String],
}

#[async_trait::async_trait]
impl OperatorWrite for SkillPlan<'_> {
    type Output = ();

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
    /// Written in one transaction.
    pub requests: Vec<WireRequest>,
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
