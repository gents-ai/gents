use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use gents::store::config_client::GraphqlEndpoint;
use gents::store::graphql::escape_graphql_string;
use gents_protocol::node_readiness::{
    AgentReadinessEntry, AgentReadinessState, NodeReadinessSnapshot,
};
use gents_protocol::operations::agent::{AgentCreateInput, AgentCreateOperation};
use gents_protocol::operations::StringUpdate;
use gents_protocol::row::{decode_node_readiness_snapshot, NodeReadinessRow};
use serde_json::Value;

use crate::config_writes::ConfigAccess;
use crate::shared::StoredInitConfig;

const DISPLAY_NAME: &str = "Yolo";
const ASSIGN_WAIT: Duration = Duration::from_secs(30);
const DESCRIPTION: &str =
    "General-purpose agent with The Engineer's tools, opened by `gents --yolo`";
const SYSTEM_PROMPT: &str = "You are a general-purpose agent on this node, working directly for its user. \
You have the same tools as The Engineer: commands run as the user, and the tool root is not a sandbox. \
Do what you are asked, completely and without ceremony. Read before you change anything, keep changes \
to what was asked, and ask before anything destructive or hard to undo.";

/// Returns the yolo agent's DID. An agent's DID is derived
/// from a fresh creation nonce, so a later run finds the yolo agent by its display name and
/// description rather than make another. It clones the node's default agent, The Engineer on
/// a home that `gents` set up, so it runs with the same Tools, its self-configuration included.
pub(super) async fn ensure_agent(
    home: &Path,
    config: &StoredInitConfig,
    graphql: &GraphqlEndpoint,
) -> Result<String> {
    let access = ConfigAccess::Graphql(graphql.clone());
    let node_did = config.node_did.as_str();
    let source = default_agent(&access, node_did).await?;
    let rows = crate::graphql_rows(
        &access,
        "Agent",
        &format!(
            r#"{{ Agent(filter: {{node_did: {{_eq: "{node}"}}}}) {{ agent_did display_name description enabled inference_profile_id created_at }} }}"#,
            node = escape_graphql_string(node_did),
        ),
    )
    .await?;
    let mut yolos: Vec<&Value> = rows
        .iter()
        .filter(|row| row["display_name"] == DISPLAY_NAME && row["description"] == DESCRIPTION)
        .collect();
    yolos.sort_by_key(|row| (row["created_at"].as_str(), row["agent_did"].as_str()));
    if let Some(agent) = yolos.iter().find(|row| row["enabled"] == Value::Bool(true)) {
        let yolo = agent["agent_did"]
            .as_str()
            .context("the yolo agent row has no agent_did")?;
        ensure_root_reader(&access, node_did, yolo, &source).await?;
        grant_self_configuration(&access, node_did, yolo, &source).await?;
        wait_until_ready(&access, node_did, yolo).await?;
        return Ok(yolo.to_owned());
    }
    if let Some(disabled) = yolos.first().and_then(|row| row["agent_did"].as_str()) {
        bail!(
            "the yolo agent {disabled} is disabled. Remove it with `gents config agent rm {disabled} --home {home}`, then run `gents --yolo` again to make a new one.",
            home = crate::shell_word(home)
        );
    }
    let profile_id = rows
        .iter()
        .find(|row| row["agent_did"] == source.as_str())
        .and_then(|agent| agent["inference_profile_id"].as_str())
        .with_context(|| {
            format!("this home has no default agent {source} with a profile to give the yolo agent")
        })?
        .to_owned();
    let identity_binding = gents_desktop_core::agent_identity::provision(graphql, node_did).await?;
    let yolo = identity_binding.agent_did.clone();
    let made = gents::operations::agent::create(
        &access,
        node_did,
        identity_binding,
        &AgentCreateInput {
            operation: AgentCreateOperation::Clone,
            clone_from: Some(source.clone()),
            display_name: DISPLAY_NAME.into(),
            description: StringUpdate::Set(DESCRIPTION.into()),
            system_prompt: StringUpdate::Set(SYSTEM_PROMPT.into()),
            profile_id,
            preset: None,
            root: StringUpdate::Omitted,
            make_default: false,
        },
        &Default::default(),
    )
    .await?;
    ensure!(
        made.agent_did == yolo,
        "the yolo agent was made as {:?}, not {yolo}",
        made.agent_did
    );
    ensure_root_reader(&access, node_did, &yolo, &source).await?;
    grant_self_configuration(&access, node_did, &yolo, &source).await?;
    eprintln!("Made the yolo agent {yolo}, with the tools of {source}.");
    wait_until_ready(&access, node_did, &yolo).await?;
    Ok(yolo)
}

/// The node's default agent, as its Node document names it.
pub(crate) async fn default_agent(access: &ConfigAccess, node_did: &str) -> Result<String> {
    let rows = crate::graphql_rows(
        access,
        "Node",
        &format!(
            r#"{{ Node(filter: {{node_did: {{_eq: "{}"}}}}, limit: 1) {{ default_agent_did }} }}"#,
            escape_graphql_string(node_did)
        ),
    )
    .await?;
    rows.first()
        .and_then(|row| row["default_agent_did"].as_str())
        .map(str::trim)
        .filter(|did| !did.is_empty())
        .map(str::to_owned)
        .with_context(|| format!("node {node_did} names no default agent"))
}

/// The yolo agent configures itself (Lean `SelfConfig.yoloGroup`): a clone
/// drops its source's self-configuration, so the User, running this command,
/// grants it the default agent's self-config group and write on its own
/// configuration, and nothing beyond it. Granting again changes nothing, so a
/// yolo agent made before this grant existed gets it on its next run.
async fn grant_self_configuration(
    access: &ConfigAccess,
    node_did: &str,
    yolo: &str,
    source: &str,
) -> Result<()> {
    gents::tools::builtin::self_config::grant_own_self_configuration(access, node_did, yolo, source)
        .await
        .with_context(|| format!("granting the yolo agent {yolo} its own self-configuration"))
}

/// Gives the yolo agent reader on the published WorkspaceRoot covering its tool root, the
/// grant a new Agent DID lacks and only a User makes; this command runs as the User who asked
/// for an agent with these tools. Adding a relationship the agent already holds changes
/// nothing, so a yolo agent made before this grant existed gets it on its next run.
async fn ensure_root_reader(
    access: &ConfigAccess,
    node_did: &str,
    yolo: &str,
    source: &str,
) -> Result<()> {
    use gents::tools::surface::ToolRootReader;
    match gents::tools::surface::agent_tool_root_reader(access, node_did, yolo).await? {
        ToolRootReader::NotNeeded => Ok(()),
        ToolRootReader::Published(root) => {
            gents::tools::surface::grant_workspace_root_reader(access, &root, yolo)
                .await
                .with_context(|| format!("granting the yolo agent {yolo} reader on {root}"))
        }
        ToolRootReader::Unpublished(root) => bail!(
            "the yolo agent {yolo} needs reader on the WorkspaceRoot covering {source}'s tool root {root}, and no published root covers it"
        ),
    }
}

/// The runtime takes on a new agent when it next reconciles, admits it again once its root
/// reader grant lands, and refuses a request for one it has not made ready. Only the
/// NodeReadiness document it then publishes says so to another process, so this bounded wait
/// reads that document again; it blocks no runtime thread.
async fn wait_until_ready(access: &ConfigAccess, node_did: &str, agent_did: &str) -> Result<()> {
    let deadline = Instant::now() + ASSIGN_WAIT;
    loop {
        let state = readiness(access, node_did, agent_did).await?;
        if state
            .as_ref()
            .is_some_and(|entry| entry.state == AgentReadinessState::Ready)
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            let detail = match state {
                Some(entry) => format!(
                    "the runtime reports it {:?}{}",
                    entry.state,
                    entry
                        .reason
                        .map(|reason| format!(" ({reason:?})"))
                        .unwrap_or_default()
                ),
                None => "the runtime has not taken it on".to_owned(),
            };
            bail!(
                "the yolo agent {agent_did} is not ready after {} s: {detail}. Check `gents status`, then run `gents --yolo` again.",
                ASSIGN_WAIT.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn readiness(
    access: &ConfigAccess,
    node_did: &str,
    agent_did: &str,
) -> Result<Option<AgentReadinessEntry>> {
    let snapshot = node_readiness(access, node_did).await?;
    let mut agents = snapshot.into_iter().flat_map(|snapshot| snapshot.agents);
    Ok(agents.find(|agent| agent.agent_did == agent_did))
}

/// The node's latest published readiness, once its router has taken on the active generation.
pub(crate) async fn node_readiness(
    access: &ConfigAccess,
    node_did: &str,
) -> Result<Option<NodeReadinessSnapshot>> {
    let rows = crate::graphql_rows(
        access,
        "NodeReadiness",
        &format!(
            r#"{{ NodeReadiness(filter: {{node_did: {{_eq: "{}"}}}}, order: {{updated_at: DESC}}, limit: 1) {{ node_did snapshot_json updated_at }} }}"#,
            escape_graphql_string(node_did)
        ),
    )
    .await?;
    let Some(row) = rows.into_iter().next() else {
        return Ok(None);
    };
    let row: NodeReadinessRow = serde_json::from_value(row).context("reading NodeReadiness")?;
    Ok(decode_node_readiness_snapshot(&row, node_did)
        .ok()
        .filter(|snapshot| snapshot.router_generation == snapshot.active_generation))
}
