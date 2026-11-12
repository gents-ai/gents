//! `gents config pack wire`: the one writer of an installed plugin's place in an
//! agent's Tools and Context. Consent is the install's alone (the recorded
//! grant bounds every call), so wiring takes no authority flag.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use gents::documents::apply_desired_state_plan;
use gents::documents::load_agent_context_and_tools_in_txn;
use gents::pack::installed_pack_documents;
use gents::plugin::authority::describe_plugin_authority;
use gents::plugin::store::read_record;
use gents::store::collection::Collection;
use gents::store::config_client::{
    ClientSigningCapability, ConfigAccess, ConfigApplyTxn, DesiredStateApplyDocument,
    DesiredStateApplyPlan, PackApprovalError, PackApprovalScope, PackEffectApprover, PackEvidence,
    PackMutation, PackRelationship, PackRelationshipEvidence,
};
use serde_json::{json, Value};

use super::split_namespace;
use crate::cli::PackWireArgs;

#[derive(Clone)]
pub(crate) struct WireRequest {
    pub(crate) plugin: String,
    pub(crate) agent: String,
    pub(crate) targets_from: Vec<String>,
    pub(crate) remove: bool,
    pub(crate) preview: bool,
    /// The node owner approved dynamic scripts for this plugin (the init
    /// approval of its pack): wiring raises its job block from `check` to
    /// `run` (Lean `consentedDynamic`). The wire command never sets it.
    pub(crate) dynamic_consent: bool,
    /// The node owner approved this plugin calling the agent's own tools
    /// (the init approval of its pack): its selection is wired with
    /// `tool_calls: true`. The wire command never sets it.
    pub(crate) tool_calls_consent: bool,
}

#[cfg(test)]
pub(super) struct WireTestPause {
    pub(super) home: std::path::PathBuf,
    pub(super) reached: tokio::sync::Notify,
    pub(super) release: tokio::sync::Notify,
}

#[cfg(test)]
pub(super) static WIRE_TEST_PAUSE: std::sync::Mutex<Option<std::sync::Arc<WireTestPause>>> =
    std::sync::Mutex::new(None);

pub(crate) async fn wire(args: PackWireArgs) -> Result<()> {
    let (access, owner) = super::resolve_scope_owner(&args.scope).await?;
    let home = crate::home_state::resolve_home_dir(args.scope.home.as_deref());
    let request = WireRequest {
        plugin: args.plugin,
        agent: args.agent,
        targets_from: args.targets_from,
        remove: args.remove,
        preview: args.preview,
        dynamic_consent: false,
        tool_calls_consent: false,
    };
    crate::print_json(&wire_in(&access, &owner, &home, &request).await?)
}

fn explain_tools_command(agent: &str) -> String {
    format!("gents config tools explain --agent-did {agent}")
}

fn publish_root_command(node_did: &str, root: &str, agent: &str) -> String {
    format!(
        "gents config workspace-root set --graphql <url> --node-did {node_did} --path {root} --reader-agent {agent}"
    )
}

/// Wires (or with `remove`, unwires) `request.plugin` over a held `access`.
/// A preview computes the same report and writes nothing.
pub(crate) async fn wire_in(
    access: &ConfigAccess,
    owner: &str,
    home: &Path,
    request: &WireRequest,
) -> Result<Value> {
    let access = access.clone();
    let owner = owner.to_owned();
    let home = home.to_owned();
    let request = request.clone();
    tokio::spawn(async move { wire_in_owned(&access, &owner, &home, &request).await }).await?
}

async fn wire_in_owned(
    access: &ConfigAccess,
    owner: &str,
    home: &Path,
    request: &WireRequest,
) -> Result<Value> {
    if !request.preview && !request.remove {
        anyhow::ensure!(matches!(access, ConfigAccess::Local(_)),
            "cannot publish a plugin pin through GraphQL because the commit outcome may be unknown; stop the runtime serving the owning node home, then run `gents config pack wire {} --agent {} --home <node-home>` without --graphql",
            request.plugin, request.agent);
    }
    let home_for_read = home.to_owned();
    let plugin_name = request.plugin.clone();
    let (_store_guard, plugin) = tokio::task::spawn_blocking(move || {
        let guard = gents::plugin::store::try_lock_store(&home_for_read)?;
        let (namespace, name) = split_namespace(&plugin_name);
        let plugin = read_record(&home_for_read, namespace, name).with_context(|| {
            format!("{plugin_name} is not installed. Next: gents config pack install <pack>")
        })?;
        Ok::<_, anyhow::Error>((guard, plugin))
    })
    .await??;
    #[cfg(test)]
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

    let mut plan = gents::pack::wiring::PluginWiring::read(
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
        .then_some(gents::tools::background::job::settings::consent_dynamic_run as _);
    plan.tool_calls_consent = request.tool_calls_consent;
    plan.next_step = explain_tools_command(&request.agent);
    let report = if request.preview {
        access
            .transact_readonly("cli.pack.wire.preview", |txn| {
                let plan = &plan;
                Box::pin(async move { plan.apply(txn).await })
            })
            .await?
    } else if matches!(access, ConfigAccess::Local(_)) {
        use gents::NodeIdentity as _;
        let client = crate::home_state::load_client_identity().await?;
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
                Box::pin(async move { plan.apply(txn.approved_interpreter()?).await })
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
                Box::pin(async move { plan.apply(txn).await })
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

/// What the plugins of `manifest` and its `dependencies` ask for beyond a
/// sealed sandbox, as `[{plugin, asks}]`: what an install grants only with
/// the installer's consent.
pub(crate) fn requested_authority(
    manifest: &gents::pack::PackManifest,
    dependencies: &[&gents::pack::PackManifest],
) -> Result<Value> {
    Ok(super::review::identity_and_authority(manifest, dependencies)?["authority"].take())
}

fn grant_reader_command(root: &str, agent: &str) -> String {
    format!(
        "gents config workspace-root grant-reader --graphql <url> --path {root} --agent-did {agent}"
    )
}

/// The agents `pack` would run under its installer's root on the node
/// `node_did`, read from its archive alone: what a starter pack's status
/// shows before any node plan exists. It is a display, not the grant's
/// source: an approval carries these rows and the grant is their
/// intersection with what the install recorded
/// ([`grant_pack_root_readers`]). A config that does not load is an error,
/// never "no such agents": what was not read was not shown.
pub(crate) fn listed_root_agents(
    pack: &super::PackSource,
    node_did: &str,
) -> Result<Vec<gents::pack::InstallerRootAgent>> {
    let coordinate = format!(
        "{}/{}",
        pack.manifest().metadata.namespace,
        pack.manifest().name
    );
    if pack.manifest().config.is_none() {
        return Ok(Vec::new());
    }
    gents::pack::load_pack_authoring_config(
        pack.manifest(),
        &gents::pack::PackInstallOptions {
            node_did: node_did.to_owned(),
        },
        &|path| Ok(pack.asset(path)?.to_vec()),
        &|name| std::env::var(name).ok(),
    )
    .and_then(|authored| {
        let tools = gents::pack::tools_taking_installer_root(&authored.documents.tools)?;
        // An authored agent names a slot, not a DID, so it is read as written.
        let rows = |key: &str| authored.value[key].as_array().into_iter().flatten();
        Ok(rows("agents")
            .filter_map(|agent| {
                let selected = rows("contexts")
                    .find(|context| {
                        context["context_id"].is_string()
                            && context["context_id"] == agent["context_id"]
                    })
                    .and_then(|context| context["tools_id"].as_str())
                    .filter(|selected| tools.iter().any(|id| id == selected))?;
                Some(gents::pack::InstallerRootAgent {
                    coordinate: coordinate.clone(),
                    agent: agent["agent_slot"]
                        .as_str()
                        .or(agent["agent_did"].as_str())?
                        .to_owned(),
                    name: agent["display_name"].as_str().map(ToOwned::to_owned),
                    tools: selected.to_owned(),
                })
            })
            .collect())
    })
    .with_context(|| {
        format!(
            "{coordinate} did not load for its host tool review, so the folder its agents would read is unknown"
        )
    })
}

/// The agents an install runs under its installer's root, from the install's
/// own plan: those of the packs it renders, as its prepare step gave them
/// the root, and those of each dependency it keeps, as that dependency's
/// installation record has them. Nothing is rendered a second time.
pub(crate) async fn planned_root_agents(
    access: &ConfigAccess,
    owner: &str,
    root: &gents::pack::PackManifest,
    prepared: &gents::pack::PreparedDocumentPackInstall,
) -> Result<Vec<gents::pack::InstallerRootAgent>> {
    let mut planned = prepared.installer_root_agents(root);
    for coordinate in prepared.kept_dependencies() {
        for agent in pack_root_agents(access, owner, coordinate).await? {
            if agent.standing == gents::pack::PackAgentRoot::Installer {
                planned.push(gents::pack::InstallerRootAgent {
                    coordinate: coordinate.to_owned(),
                    agent: agent.key,
                    name: None,
                    tools: agent.tools,
                });
            }
        }
    }
    Ok(planned)
}

/// One agent of an installed pack that runs host tools under a published
/// root: who it is, where its Tools run now, and how the pack's record says
/// it came by that root.
struct PackRootAgent {
    agent_did: String,
    /// The agent as a review names it: its slot, or its DID.
    key: String,
    tools: String,
    root: String,
    reader: gents::tools::surface::ToolRootReader,
    standing: gents::pack::PackAgentRoot,
}

/// The agents the installed pack `coordinate` ships that run host tools:
/// the pack's record ([`gents::pack::pack_root_record`]) names them and how
/// each Tools document came by its root, and the root admission owner reads
/// where each runs now. The record's reading stays with its owner; this only
/// joins the two.
async fn pack_root_agents(
    access: &ConfigAccess,
    owner: &str,
    coordinate: &str,
) -> Result<Vec<PackRootAgent>> {
    let record = gents::pack::pack_root_record(access, owner, coordinate).await?;
    Ok(
        gents::tools::surface::agents_tool_root_readers(access, owner, record.agents())
            .await?
            .into_iter()
            .map(|(agent_did, tools, root, reader)| PackRootAgent {
                key: record.agent_key(&agent_did),
                standing: record.standing(&tools),
                agent_did,
                tools,
                root,
                reader,
            })
            .collect(),
    )
}

/// What a review shows of `agents`, the ones an install by `installer` (the
/// node's User when `None`) runs under its root: the root, the reader they
/// need there, and each (pack, agent) row, which an approval names back.
/// `None` when there are none, the installer has no root, or no reader is
/// needed.
pub(crate) async fn root_review(
    access: &ConfigAccess,
    owner: &str,
    installer: Option<&str>,
    agents: &[gents::pack::InstallerRootAgent],
) -> Result<Option<Value>> {
    use gents::tools::surface::ToolRootReader;
    if agents.is_empty() {
        return Ok(None);
    }
    let Some(root) = gents::pack::installer_tool_root(access, owner, installer).await? else {
        return Ok(None);
    };
    let mut tools: Vec<&String> = agents.iter().map(|agent| &agent.tools).collect();
    tools.dedup();
    let mut review = json!({
        "tools": tools,
        "agents": agents.iter().map(|agent| agent.name.as_ref().unwrap_or(&agent.agent)).collect::<Vec<_>>(),
        "rows": root_agent_rows(agents),
        "root": root,
    });
    match gents::tools::surface::tool_root_reader(access, owner, &root).await? {
        ToolRootReader::NotNeeded => return Ok(None),
        ToolRootReader::Published(published) => review["reader_on"] = published.into(),
        ToolRootReader::Unpublished(_) => review["unpublished_root"] = root.into(),
    }
    Ok(Some(review))
}

/// `agents` as a report lists them: `[{pack, agent, name, tools}]`.
pub(crate) fn root_agent_rows(agents: &[gents::pack::InstallerRootAgent]) -> Value {
    agents
        .iter()
        .map(|agent| {
            json!({"pack": agent.coordinate, "agent": agent.agent, "name": agent.name, "tools": agent.tools})
        })
        .collect()
}

/// The field an install's result carries its plan's rows under, from the
/// install to the grant that follows it: over HTTP too, where the install
/// runs in the runtime and the grant in the client. It is not part of a
/// receipt: the command that grants takes it out and lists
/// `host_tool_roots` instead.
const PLANNED_ROOT_AGENTS: &str = "installer_root_agents";

/// Puts an install's planned rows on its `result` ([`PLANNED_ROOT_AGENTS`]).
pub(crate) fn attach_planned_root_agents(
    result: &mut Value,
    planned: &[gents::pack::InstallerRootAgent],
) {
    result[PLANNED_ROOT_AGENTS] = root_agent_rows(planned);
}

/// Takes the planned rows off an install's `result`, as `(pack, agent)`
/// pairs: what an approval of that install alone was shown.
pub(crate) fn take_planned_root_agents(result: &mut Value) -> Vec<(String, String)> {
    let rows = result
        .as_object_mut()
        .and_then(|result| result.remove(PLANNED_ROOT_AGENTS))
        .unwrap_or_default();
    shown_root_agents(&rows)
}

/// The `PACK=AGENT` rows a review named back (`--root-agent`), as pairs. A
/// value that is not one is refused: dropped, it would silently leave an
/// agent the approver meant ungranted.
pub(crate) fn reviewed_root_agents(reviewed: &[String]) -> Result<Vec<(String, String)>> {
    reviewed
        .iter()
        .map(|pair| {
            let (pack, agent) = pair
                .split_once('=')
                .filter(|(pack, agent)| !pack.is_empty() && !agent.is_empty())
                .with_context(|| {
                    format!("--root-agent {pair:?} is not NAMESPACE/PACK=AGENT, a row of a preview's host_tool_roots.rows")
                })?;
            Ok((pack.to_owned(), agent.to_owned()))
        })
        .collect()
}

/// The `(pack, agent)` pairs of report rows ([`root_agent_rows`]).
fn shown_root_agents(rows: &Value) -> Vec<(String, String)> {
    rows.as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| {
            Some((
                row["pack"].as_str()?.to_owned(),
                row["agent"].as_str()?.to_owned(),
            ))
        })
        .collect()
}

/// The pack coordinate whose reader grants a test makes fail.
#[cfg(test)]
pub(crate) static FAIL_GRANTS_FOR: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Grants the agents of the installed packs `coordinates` (a pack and its
/// dependencies) that run under their installer's root reader on the
/// published WorkspaceRoot covering it, as the User holding `access`: a new
/// agent lacks that grant, only a User makes it, and without it the runtime
/// reports the agent unavailable.
///
/// What is granted is decided by two facts and no rendering. The pack's own
/// installation record says which of its agents select Tools of that pack
/// whose root its install supplied ([`gents::pack::pack_root_record`]); and
/// `shown` holds the `(pack, agent)` pairs the approver's review listed. An
/// agent is granted only when it is both, the install was `approved`, and
/// its Tools still run under `supplied`, the root this install gives
/// ([`gents::pack::installer_tool_root`]). A dependency installed before
/// keeps its record, so repeating an install whose grant failed makes the
/// grant its first attempt missed; granting a reader already held changes
/// nothing. Every other agent with host tools is listed with why and the
/// User command that grants it. Returns one entry per agent.
pub(crate) async fn grant_pack_root_readers(
    access: &ConfigAccess,
    owner: &str,
    coordinates: &[String],
    shown: &[(String, String)],
    supplied: Option<&str>,
    approved: bool,
) -> Result<Vec<Value>> {
    use gents::pack::PackAgentRoot;
    use gents::tools::surface::ToolRootReader;
    let mut granted = Vec::new();
    let mut seen = Vec::new();
    #[cfg(test)]
    let failing = FAIL_GRANTS_FOR
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|failing| coordinates.contains(failing));
    for coordinate in coordinates {
        for row in pack_root_agents(access, owner, coordinate).await? {
            if seen.contains(&row.agent_did) {
                continue;
            }
            seen.push(row.agent_did.clone());
            let (agent, root) = (row.agent_did, row.root);
            let published = match row.reader {
                ToolRootReader::Published(published) => published,
                ToolRootReader::Unpublished(root) => {
                    let next = publish_root_command(owner, &root, &agent);
                    granted.push(json!({"agent": agent, "unpublished_root": root, "next": next}));
                    continue;
                }
                ToolRootReader::NotNeeded => continue,
            };
            let listed = shown
                .iter()
                .any(|(pack, shown)| pack == coordinate && *shown == row.key);
            let reason = match row.standing {
                PackAgentRoot::Own => "its Tools name their own root",
                PackAgentRoot::OtherPack => "it uses another pack's Tools",
                PackAgentRoot::Installer if supplied.is_none() => {
                    "this install supplies no tool root"
                }
                PackAgentRoot::Installer if supplied != Some(root.as_str()) => {
                    "its tool root changed since its install supplied it"
                }
                PackAgentRoot::Installer if !listed => "the install's review did not list it",
                PackAgentRoot::Installer if !approved => "the install was not approved to grant it",
                PackAgentRoot::Installer => {
                    let grant = gents::tools::surface::grant_workspace_root_reader(
                        access, &published, &agent,
                    )
                    .await
                    .map_err(anyhow::Error::from);
                    #[cfg(test)]
                    let grant = if failing {
                        Err(anyhow::anyhow!("simulated reader grant failure"))
                    } else {
                        grant
                    };
                    // One agent's failed grant does not hide the others':
                    // each is reported with the command that makes it.
                    granted.push(match grant {
                        Ok(()) => json!({"agent": agent, "root": root, "reader_on": published}),
                        Err(error) => json!({
                            "agent": agent,
                            "root": root,
                            "failed": format!("granting {agent} reader on {published}: {error:#}"),
                            "next": grant_reader_command(&published, &agent),
                        }),
                    });
                    continue;
                }
            };
            granted.push(json!({
                "agent": agent,
                "root": root,
                "not_granted": format!("{agent} was not granted reader on {published}: {reason}. It is unavailable until a User grants it."),
                "next": grant_reader_command(&published, &agent),
            }));
        }
    }
    Ok(granted)
}

/// Attaches the Skill documents the installed pack `coordinate` (archive
/// `digest`) ships to `agent`'s Context, through the transaction a wire
/// writes with. Returns the skills the pack ships.
pub(crate) async fn attach_pack_skills(
    access: &ConfigAccess,
    owner: &str,
    agent: &str,
    coordinate: &str,
    digest: &str,
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
        use gents::NodeIdentity as _;
        let client = crate::home_state::load_client_identity().await?;
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
        apply_desired_state_plan(txn, &plan).await?;
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
        gents::store::config_client::prepare_pack_evidence_from_history(
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
        gents::store::config_client::prepare_pack_relationship(
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
        Ok(gents::store::config_client::mint_recreate_identity_timestamp())
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

#[cfg(test)]
mod next_command_tests {
    use clap::Parser as _;

    /// An install's planned rows travel on its result to the grant and are
    /// taken off it there, so they reach the grant from any install path
    /// and never stay in a receipt.
    #[test]
    fn planned_root_agents_travel_on_the_result_and_leave_it() {
        let planned = [gents::pack::InstallerRootAgent {
            coordinate: "acme/hands".to_owned(),
            agent: "hands-worker".to_owned(),
            name: Some("Hands worker".to_owned()),
            tools: "hands-tools".to_owned(),
        }];
        let mut result = serde_json::json!({"pack": "hands"});
        super::attach_planned_root_agents(&mut result, &planned);
        assert_eq!(
            super::take_planned_root_agents(&mut result),
            [("acme/hands".to_owned(), "hands-worker".to_owned())]
        );
        assert_eq!(result, serde_json::json!({"pack": "hands"}));
        assert!(super::take_planned_root_agents(&mut result).is_empty());
    }

    /// A reviewed row that is not `PACK=AGENT` is refused, not dropped.
    #[test]
    fn a_malformed_reviewed_row_is_refused() {
        assert_eq!(
            super::reviewed_root_agents(&["acme/hands=hands-worker".to_owned()]).unwrap(),
            [("acme/hands".to_owned(), "hands-worker".to_owned())]
        );
        for malformed in ["acme/hands", "=hands-worker", "acme/hands="] {
            let refusal = super::reviewed_root_agents(&[malformed.to_owned()]).unwrap_err();
            assert!(
                format!("{refusal:#}").contains("NAMESPACE/PACK=AGENT"),
                "{refusal:#}"
            );
        }
    }

    #[test]
    fn the_commands_wiring_names_parse() {
        for command in [
            super::explain_tools_command("did:key:agent"),
            super::publish_root_command("did:key:node", "/srv/root", "did:key:agent"),
        ] {
            let arguments = command.split(' ').map(|word| match word {
                "<url>" => "http://127.0.0.1:9191/api/v1/graphql",
                word => word,
            });
            crate::cli::Cli::try_parse_from(arguments)
                .unwrap_or_else(|error| panic!("{command}: {error}"));
        }
    }
}
