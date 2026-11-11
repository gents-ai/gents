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

/// The Tools one pack of an install ships that select host tools and name no
/// root: the ones the install runs under its installer's own root, and whose
/// agents an approved install grants reader there. This is what a review
/// lists, and the grant is made from the same value, so the two cannot name
/// different Tools.
pub(crate) struct InstallerRootTools {
    /// The pack, as `namespace/name`.
    pub(crate) coordinate: String,
    /// By `tools_id`, as the install writes them on this node.
    pub(crate) tools: Vec<String>,
    /// The agents whose Context selects one of them, by the name the pack
    /// gives each: who a review says will read the folder.
    pub(crate) agents: Vec<String>,
    /// Why the pack's config did not load for the review, when it did not:
    /// nothing of it is listed, so nothing of it is granted.
    pub(crate) unread: Option<String>,
}

/// [`InstallerRootTools`] of `pack` on the node `node_did`. The config is
/// rendered with that node's DID, as its install renders it, so a Tools id
/// that depends on the node is the id the installed document has. A config
/// that does not load is reported `unread`, never as shipping no such Tools.
pub(crate) fn tools_taking_installer_root(
    pack: &super::PackSource,
    node_did: &str,
) -> InstallerRootTools {
    let coordinate = format!(
        "{}/{}",
        pack.manifest().metadata.namespace,
        pack.manifest().name
    );
    let mut listed = InstallerRootTools {
        coordinate,
        tools: Vec::new(),
        agents: Vec::new(),
        unread: None,
    };
    if pack.manifest().config.is_none() {
        return listed;
    }
    let loaded = gents::pack::load_pack_authoring_config(
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
        let agents = rows("agents")
            .filter(|agent| {
                rows("contexts").any(|context| {
                    context["context_id"].is_string()
                        && context["context_id"] == agent["context_id"]
                        && context["tools_id"]
                            .as_str()
                            .is_some_and(|selected| tools.iter().any(|id| id == selected))
                })
            })
            .filter_map(|agent| {
                ["display_name", "agent_slot", "agent_did"]
                    .iter()
                    .find_map(|field| agent[*field].as_str())
                    .map(ToOwned::to_owned)
            })
            .collect();
        Ok((tools, agents))
    });
    match loaded {
        Ok((tools, agents)) => {
            listed.tools = tools;
            listed.agents = agents;
        }
        Err(error) => {
            listed.unread = Some(format!(
                "{} did not load for its host tool review, so the folder its agents would read is unknown: {error:#}",
                listed.coordinate
            ));
        }
    }
    listed
}

/// The root an install by `installer` (the node's User when `None`) supplies
/// to the Tools `listed` (of the root pack and its dependencies), and the
/// reader their agents need there: what a preview lists and an approval
/// covers. `None` when no such Tools are listed, the installer has no root,
/// or no reader is needed.
pub(crate) async fn supplied_tool_root(
    access: &ConfigAccess,
    owner: &str,
    installer: Option<&str>,
    listed: &[InstallerRootTools],
) -> Result<Option<Value>> {
    use gents::tools::surface::ToolRootReader;
    let tools: Vec<&String> = listed.iter().flat_map(|pack| &pack.tools).collect();
    let agents: Vec<&String> = listed.iter().flat_map(|pack| &pack.agents).collect();
    if tools.is_empty() {
        return Ok(None);
    }
    let Some(root) = gents::pack::installer_tool_root(access, owner, installer).await? else {
        return Ok(None);
    };
    Ok(
        match gents::tools::surface::tool_root_reader(access, owner, &root).await? {
            ToolRootReader::NotNeeded => None,
            ToolRootReader::Published(published) => Some(
                json!({"tools": tools, "agents": agents, "root": root, "reader_on": published}),
            ),
            ToolRootReader::Unpublished(_) => Some(
                json!({"tools": tools, "agents": agents, "root": root, "unpublished_root": root}),
            ),
        },
    )
}

/// The pack coordinate whose reader grants a test makes fail.
#[cfg(test)]
pub(crate) static FAIL_GRANTS_FOR: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Grants each agent the installed packs `listed` (a pack and its
/// dependencies) ship with host tools reader on the published WorkspaceRoot
/// covering its tool root, as the User holding `access`: a new agent lacks
/// that grant, only a User makes it, and without it the runtime reports the
/// agent unavailable. Every dependency is covered, installed by this install
/// or before it, so repeating an install whose grant failed makes the grant
/// its first attempt missed; granting a reader that is already held changes
/// nothing.
///
/// The grant is the installer's consent to its own root and nothing else: it
/// is made only when the install was `approved`, and only for an agent whose
/// Tools document the review listed for the pack that installed it (the
/// rootless Tools the install gave its root `supplied`,
/// [`gents::pack::installer_tool_root`]). A listed id counts only as a
/// Tools document of its own pack, so another pack's Tools with the same id
/// are not it. Tools that name their own root are never granted here, even
/// when that root renders equal to the installer's: the review did not list
/// them. An agent under any other root, one an install variable set or a
/// later edit wrote, is listed with the User command that grants it.
/// Returns one entry per agent.
pub(crate) async fn grant_pack_root_readers(
    access: &ConfigAccess,
    owner: &str,
    listed: &[InstallerRootTools],
    supplied: Option<&str>,
    approved: bool,
) -> Result<Vec<Value>> {
    use gents::tools::surface::ToolRootReader;
    let mut granted = Vec::new();
    let mut agents = Vec::new();
    // The Tools documents the review listed that their own pack installed.
    let mut inherited = Vec::new();
    // Each agent's pack, when that pack did not load for the review.
    let mut unread = Vec::new();
    for pack in listed {
        for (collection, id) in installed_pack_documents(access, owner, &pack.coordinate).await? {
            match collection {
                Collection::Agent => {
                    if pack.unread.is_some() {
                        unread.push(id.clone());
                    }
                    agents.push(id);
                }
                Collection::Tools if pack.tools.contains(&id) => inherited.push(id),
                _ => {}
            }
        }
    }
    #[cfg(test)]
    let failing = FAIL_GRANTS_FOR
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|failing| listed.iter().any(|pack| pack.coordinate == *failing));
    for (agent, tools, root, reader) in
        gents::tools::surface::agents_tool_root_readers(access, owner, &agents).await?
    {
        let listed = supplied == Some(root.as_str()) && inherited.contains(&tools);
        granted.push(match reader {
            ToolRootReader::Published(published) if approved && listed => {
                let grant =
                    gents::tools::surface::grant_workspace_root_reader(access, &published, &agent)
                        .await
                        .map_err(anyhow::Error::from);
                #[cfg(test)]
                let grant = if failing {
                    Err(anyhow::anyhow!("simulated reader grant failure"))
                } else {
                    grant
                };
                // One agent's failed grant does not hide the others': each is
                // reported with the command that makes it.
                match grant {
                    Ok(()) => json!({"agent": agent, "root": root, "reader_on": published}),
                    Err(error) => json!({
                        "agent": agent,
                        "root": root,
                        "failed": format!("granting {agent} reader on {published}: {error:#}"),
                        "next": grant_reader_command(&published, &agent),
                    }),
                }
            }
            ToolRootReader::Published(published) => {
                let reason = if listed {
                    "the install was not approved to grant it"
                } else if unread.contains(&agent) {
                    "its pack's configuration did not load for the install's review, so its Tools were not listed"
                } else if supplied.is_none() {
                    "this install supplied no tool root"
                } else if supplied == Some(root.as_str()) {
                    "its Tools name that root themselves, so the install's review did not list it"
                } else {
                    "its tool root is not the one this install supplied"
                };
                json!({
                    "agent": agent,
                    "root": root,
                    "not_granted": format!("{agent} was not granted reader on {published}: {reason}. It is unavailable until a User grants it."),
                    "next": grant_reader_command(&published, &agent),
                })
            }
            ToolRootReader::Unpublished(root) => {
                let next = publish_root_command(owner, &root, &agent);
                json!({"agent": agent, "unpublished_root": root, "next": next})
            }
            ToolRootReader::NotNeeded => continue,
        });
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
