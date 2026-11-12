use anyhow::{Context, Result};
use gents_pack::pack::installation_roots::{root_agent_rows, InstallerRoots};
use gents_pack::pack::{
    InstallerRootAgent, PackAgentRoot, PackManifest, PreparedDocumentPackInstall,
};
use gents_protocol::operations::pack::{PackActor, ReaderAction, RootReader};
use gents_store::collection::Collection;
use gents_store::config_client::ConfigAccess;
use serde_json::{json, Value};

use super::ToolRootReader;

/// What an install did, or reports, about the reader one shipped Agent needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackRootReader {
    pub agent: String,
    /// The published root, or the unpublished tool root.
    pub root: String,
    pub action: ReaderAction,
}

/// Lean `Pack.Operations.readerAction` for each Agent the installed packs
/// `coordinates` (a pack and the dependencies its install put on the node)
/// ship with host tools: `actor`'s install grants reader on the published
/// root covering its tool root, or reports the step.
pub async fn pack_root_readers(
    access: &ConfigAccess,
    node_did: &str,
    coordinates: &[String],
    actor: PackActor,
) -> Result<Vec<PackRootReader>> {
    let mut agents: Vec<String> = Vec::new();
    for coordinate in coordinates {
        agents.extend(
            gents_pack::pack::installed_pack_documents(access, node_did, coordinate)
                .await?
                .into_iter()
                .filter(|(collection, _)| *collection == Collection::Agent)
                .map(|(_, agent)| agent),
        );
    }
    let mut readers = Vec::new();
    for (agent, _, _, reader) in super::agents_tool_root_readers(access, node_did, &agents).await? {
        let (kind, root) = match reader {
            ToolRootReader::NotNeeded => continue,
            ToolRootReader::Published(root) => (RootReader::Published, root),
            ToolRootReader::Unpublished(root) => (RootReader::Unpublished, root),
        };
        let action = gents_pack::operations::pack::reader_action(actor, kind);
        if action == ReaderAction::Grant {
            super::grant_workspace_root_reader(access, &root, &agent)
                .await
                .with_context(|| format!("granting {agent} reader on {root}"))?;
        }
        readers.push(PackRootReader {
            agent,
            root,
            action,
        });
    }
    Ok(readers)
}

/// The User command that publishes `root` with `agent` as a reader.
pub fn publish_root_command(node_did: &str, root: &str, agent: &str) -> String {
    format!(
        "gents config workspace-root set --graphql <url> --node-did {node_did} --path {root} --reader-agent {agent}"
    )
}

/// The User command that grants `agent` reader on the published `root`.
pub fn grant_reader_command(root: &str, agent: &str) -> String {
    format!(
        "gents config workspace-root grant-reader --graphql <url> --path {root} --agent-did {agent}"
    )
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
    reader: ToolRootReader,
    standing: PackAgentRoot,
}

/// The agents the installed pack `coordinate` ships that run host tools:
/// the pack's record ([`gents_pack::pack::pack_root_record`]) names them and
/// how each Tools document came by its root, and the root admission owner
/// reads where each runs now. The record's reading stays with its owner;
/// this only joins the two.
async fn pack_root_agents(
    access: &ConfigAccess,
    owner: &str,
    coordinate: &str,
) -> Result<Vec<PackRootAgent>> {
    let record = gents_pack::pack::pack_root_record(access, owner, coordinate).await?;
    Ok(
        super::agents_tool_root_readers(access, owner, record.agents())
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

/// The root admission an install consults before it writes
/// ([`InstallerRoots`]).
pub struct PackRoots;

#[async_trait::async_trait]
impl InstallerRoots for PackRoots {
    async fn planned_root_agents(
        &self,
        access: &ConfigAccess,
        owner: &str,
        root: &PackManifest,
        prepared: &PreparedDocumentPackInstall,
    ) -> Result<Vec<InstallerRootAgent>> {
        planned_root_agents(access, owner, root, prepared).await
    }

    async fn root_review(
        &self,
        access: &ConfigAccess,
        owner: &str,
        installer: Option<&str>,
        agents: &[InstallerRootAgent],
    ) -> Result<Option<Value>> {
        root_review(access, owner, installer, agents).await
    }
}

/// The agents an install runs under its installer's root, from the install's
/// own plan: those of the packs it renders, as its prepare step gave them
/// the root, and those of each dependency it keeps, as that dependency's
/// installation record has them. Nothing is rendered a second time.
pub async fn planned_root_agents(
    access: &ConfigAccess,
    owner: &str,
    root: &PackManifest,
    prepared: &PreparedDocumentPackInstall,
) -> Result<Vec<InstallerRootAgent>> {
    let mut planned = prepared.installer_root_agents(root);
    for coordinate in prepared.kept_dependencies() {
        for agent in pack_root_agents(access, owner, coordinate).await? {
            if agent.standing == PackAgentRoot::Installer {
                planned.push(InstallerRootAgent {
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

/// What a review shows of `agents`, the ones an install by `installer` (the
/// node's User when `None`) runs under its root: the root, the reader they
/// need there, and each (pack, agent) row, which an approval names back.
/// `None` when there are none, the installer has no root, or no reader is
/// needed.
pub async fn root_review(
    access: &ConfigAccess,
    owner: &str,
    installer: Option<&str>,
    agents: &[InstallerRootAgent],
) -> Result<Option<Value>> {
    if agents.is_empty() {
        return Ok(None);
    }
    let Some(root) = gents_pack::pack::installer_tool_root(access, owner, installer).await? else {
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
    match super::tool_root_reader(access, owner, &root).await? {
        ToolRootReader::NotNeeded => return Ok(None),
        ToolRootReader::Published(published) => review["reader_on"] = published.into(),
        ToolRootReader::Unpublished(_) => review["unpublished_root"] = root.into(),
    }
    Ok(Some(review))
}

/// The pack coordinates whose reader grants a test makes fail: each test
/// adds and removes its own, so tests running at once do not clear another's.
#[cfg(any(test, feature = "test-fixtures"))]
pub static FAIL_GRANTS_FOR: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Grants the agents of the installed packs `coordinates` (a pack and its
/// dependencies) that run under their installer's root reader on the
/// published WorkspaceRoot covering it, as the User holding `access`: a new
/// agent lacks that grant, only a User makes it, and without it the runtime
/// reports the agent unavailable.
///
/// What is granted is decided by two facts and no rendering. The pack's own
/// installation record says which of its agents select Tools of that pack
/// whose root its install supplied ([`gents_pack::pack::pack_root_record`]);
/// and `shown` holds the `(pack, agent)` pairs the approver's review listed.
/// An agent is granted only when it is both, the install was `approved`, and
/// its Tools still run under `supplied`, the root this install gives
/// ([`gents_pack::pack::installer_tool_root`]). A dependency installed before
/// keeps its record, so repeating an install whose grant failed makes the
/// grant its first attempt missed; granting a reader already held changes
/// nothing. Every other agent with host tools is listed with why and the
/// User command that grants it. Returns one entry per agent.
pub async fn grant_pack_root_readers(
    access: &ConfigAccess,
    owner: &str,
    coordinates: &[String],
    shown: &[(String, String)],
    supplied: Option<&str>,
    approved: bool,
) -> Result<Vec<Value>> {
    let mut granted = Vec::new();
    let mut seen = Vec::new();
    #[cfg(any(test, feature = "test-fixtures"))]
    let failing = FAIL_GRANTS_FOR
        .lock()
        .unwrap()
        .iter()
        .any(|failing| coordinates.contains(failing));
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
                    let grant = super::grant_workspace_root_reader(access, &published, &agent)
                        .await
                        .map_err(anyhow::Error::from);
                    #[cfg(any(test, feature = "test-fixtures"))]
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
