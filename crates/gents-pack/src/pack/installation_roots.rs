//! The installer's tool root an install hands to the agents it ships: the
//! seam through which the install consults root admission, and the rows that
//! carry what a review showed to the grant that follows the install.

use anyhow::{Context, Result};
use gents_store::config_client::ConfigAccess;
use serde_json::{json, Value};

use super::{InstallerRootAgent, PackManifest, PreparedDocumentPackInstall};

/// The root admission reads an install makes before it writes: which agents
/// it runs under its installer's root and what a review shows of them. Root
/// admission is owned above the pack layer (`gents_tools::surface`), so the
/// install takes it as an argument.
#[async_trait::async_trait]
pub trait InstallerRoots: Send + Sync {
    /// The agents the install planned in `prepared` runs under its
    /// installer's root.
    async fn planned_root_agents(
        &self,
        access: &ConfigAccess,
        owner: &str,
        root: &PackManifest,
        prepared: &PreparedDocumentPackInstall,
    ) -> Result<Vec<InstallerRootAgent>>;

    /// What a review shows of `agents` under `installer`'s root, or `None`
    /// when no reader is needed.
    async fn root_review(
        &self,
        access: &ConfigAccess,
        owner: &str,
        installer: Option<&str>,
        agents: &[InstallerRootAgent],
    ) -> Result<Option<Value>>;
}

/// `agents` as a report lists them: `[{pack, agent, name, tools}]`.
pub fn root_agent_rows(agents: &[InstallerRootAgent]) -> Value {
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
pub fn attach_planned_root_agents(result: &mut Value, planned: &[InstallerRootAgent]) {
    result[PLANNED_ROOT_AGENTS] = root_agent_rows(planned);
}

/// Takes the planned rows off an install's `result`, as `(pack, agent)`
/// pairs: what an approval of that install alone was shown.
pub fn take_planned_root_agents(result: &mut Value) -> Vec<(String, String)> {
    let rows = result
        .as_object_mut()
        .and_then(|result| result.remove(PLANNED_ROOT_AGENTS))
        .unwrap_or_default();
    shown_root_agents(&rows)
}

/// The `PACK=AGENT` rows a review named back (`--root-agent`), as pairs. A
/// value that is not one is refused: dropped, it would silently leave an
/// agent the approver meant ungranted.
pub fn reviewed_root_agents(reviewed: &[String]) -> Result<Vec<(String, String)>> {
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

#[cfg(test)]
mod tests {
    use super::super::InstallerRootAgent;

    /// An install's planned rows travel on its result to the grant and are
    /// taken off it there, so they reach the grant from any install path
    /// and never stay in a receipt.
    #[test]
    fn planned_root_agents_travel_on_the_result_and_leave_it() {
        let planned = [InstallerRootAgent {
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
}
