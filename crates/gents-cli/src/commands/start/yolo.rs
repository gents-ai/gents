use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use gents::store::config_client::GraphqlEndpoint;
use gents::store::graphql::escape_graphql_string;
use gents::tools::builtin::self_config::agent::derive_agent_id;
use gents::tools::builtin::self_config::{configure_agent, ConfigureAgentParams, StringUpdate};
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

/// Returns the yolo agent's ID and whether this call made it. The agent materializer derives
/// an ID from the display name, so the first yolo agent gets this one and later runs find it
/// rather than make another. It clones the home's default agent, The Engineer on a home that
/// `gents` set up, so it runs with the same Tools document.
pub(super) async fn ensure_agent(
    home: &Path,
    config: &StoredInitConfig,
    graphql: &GraphqlEndpoint,
) -> Result<(String, bool)> {
    let access = ConfigAccess::Graphql(graphql.clone());
    let node_did = config.node_did.as_str();
    let yolo = derive_agent_id(node_did, DISPLAY_NAME, &BTreeMap::new());
    let source = gents::default_agent_id_for_node(node_did);
    let rows = crate::graphql_rows(
        &access,
        "Agent",
        &format!(
            r#"{{ Agent(filter: {{node_did: {{_eq: "{node}"}}, agent_id: {{_in: ["{yolo}", "{source}"]}}}}) {{ agent_id enabled inference_profile_id }} }}"#,
            node = escape_graphql_string(node_did),
            yolo = escape_graphql_string(&yolo),
            source = escape_graphql_string(&source),
        ),
    )
    .await?;
    let find = |id: &str| rows.iter().find(|row| row["agent_id"] == id);
    if let Some(agent) = find(&yolo) {
        if agent["enabled"] == Value::Bool(false) {
            bail!(
                "the yolo agent {yolo} is disabled. Remove it with `gents config agent rm {yolo} --home {home}`, then run `gents --yolo` again to make a new one.",
                home = crate::shell_word(home)
            );
        }
        return Ok((yolo, false));
    }
    let profile_id = find(&source)
        .and_then(|agent| agent["inference_profile_id"].as_str())
        .with_context(|| {
            format!("this home has no default agent {source} with a profile to give the yolo agent")
        })?
        .to_owned();
    let identity = crate::load_initialized_home_identity(home, config)?;
    let made = configure_agent(
        &access,
        node_did,
        identity.as_ref(),
        &ConfigureAgentParams {
            action: "clone".into(),
            clone_from: Some(source.clone()),
            display_name: StringUpdate::Set(DISPLAY_NAME.into()),
            description: StringUpdate::Set(DESCRIPTION.into()),
            system_prompt: StringUpdate::Set(SYSTEM_PROMPT.into()),
            profile_id: StringUpdate::Set(profile_id),
            ..Default::default()
        },
        &Default::default(),
    )
    .await?;
    let made: Value = serde_json::from_str(&made).context("reading the made agent")?;
    ensure!(
        made["agent_id"] == yolo.as_str(),
        "the yolo agent was made as {}, not {yolo}",
        made["agent_id"]
    );
    eprintln!("Made the yolo agent {yolo}, with the tools of {source}.");
    wait_until_assigned(&access, node_did, &yolo).await?;
    Ok((yolo, true))
}

/// The runtime takes on a new agent when it next reconciles, and refuses a request for one it
/// has not taken on. Only the NodeReadiness document it then publishes says so to another
/// process, so this bounded wait reads that document again; it blocks no runtime thread.
async fn wait_until_assigned(access: &ConfigAccess, node_did: &str, agent_id: &str) -> Result<()> {
    let deadline = Instant::now() + ASSIGN_WAIT;
    while !assigned(access, node_did, agent_id).await? {
        if Instant::now() >= deadline {
            bail!(
                "this home's runtime has not taken on the yolo agent {agent_id} after {} s. Check `gents status`, then run `gents --yolo` again.",
                ASSIGN_WAIT.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Ok(())
}

async fn assigned(access: &ConfigAccess, node_did: &str, agent_id: &str) -> Result<bool> {
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
        return Ok(false);
    };
    let row: NodeReadinessRow = serde_json::from_value(row).context("reading NodeReadiness")?;
    Ok(
        decode_node_readiness_snapshot(&row, node_did).is_ok_and(|snapshot| {
            snapshot.router_generation == snapshot.active_generation
                && snapshot
                    .agents
                    .iter()
                    .any(|agent| agent.agent_id == agent_id)
        }),
    )
}
