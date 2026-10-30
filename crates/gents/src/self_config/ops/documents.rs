use anyhow::{anyhow, Context, Result};
use serde_json::{json, Map, Value};

use crate::config_client::patch::{diff_docs, FieldDelta, SelfConfigTarget};
use crate::config_client::{
    read_desired_state_record_in_txn, ConfigApplyTxn, DesiredStateApplyDocument,
    DesiredStateApplyPlan,
};
use crate::documents::Tools;
use crate::graphql::escape_graphql_string;
use crate::tools::native::CommandNetworkMode;

pub fn apply_tool_grant_selection(
    tools: &mut Tools,
    enable_lsp: Option<bool>,
    network_mode: Option<CommandNetworkMode>,
) {
    if let Some(enabled) = enable_lsp {
        let integrations = tools.integrations.get_or_insert_with(Default::default);
        if enabled {
            integrations.lsp.get_or_insert_with(Default::default);
        } else {
            integrations.lsp = None;
        }
    }
    if let Some(network_mode) = network_mode {
        tools
            .host
            .get_or_insert_with(Default::default)
            .bash
            .get_or_insert_with(Default::default)
            .network_mode = Some(network_mode);
    }
}

pub fn validate_tool_network_selection(network_mode: Option<CommandNetworkMode>) -> Result<()> {
    anyhow::ensure!(
        network_mode.is_none_or(|mode| mode == CommandNetworkMode::Disabled),
        "config agent tools may only narrow network_mode to disabled"
    );
    Ok(())
}

pub(crate) fn decode_merged<T>(collection: &str, merged: &Map<String, Value>) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    serde_path_to_error::deserialize(Value::Object(merged.clone())).map_err(|error| {
        let path = error.path().to_string();
        let inner = error.inner().to_string();
        match super::super::command::contracts::field_shape(collection, &path) {
            Some(shape) => {
                let found = inner.split(", expected ").next().unwrap_or(&inner);
                let shape = shape
                    .as_str()
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| shape.to_string());
                anyhow!("{collection} field {path}: {found}; expected {shape}")
            }
            None if path != "." => anyhow!("{collection} field {path}: {inner}"),
            None => anyhow!("merged {collection} document is not valid: {inner}"),
        }
    })
}

pub(crate) fn validate_merged_selection(merged: &Map<String, Value>) -> Result<()> {
    let tools = decode_merged::<Tools>("Tools", merged)?;
    if let Some(lsp) = tools
        .integrations
        .as_ref()
        .and_then(|group| group.lsp.as_ref())
    {
        crate::tools::native::lsp::LspConfigDocument::parse_self_config(lsp.config.as_deref())
            .map_err(anyhow::Error::msg)?;
    }
    tools.validate()
}
pub(crate) async fn read_owned_doc(
    txn: &ConfigApplyTxn<'_>,
    target: SelfConfigTarget,
    owner: &str,
    id: &str,
) -> Result<Option<(String, Map<String, Value>)>> {
    read_desired_state_record_in_txn(txn, target.collection(), owner, id)
        .await?
        .map(|(id, value)| {
            let doc = value.as_object().context("config object required")?.clone();
            Ok((id, doc))
        })
        .transpose()
}
/// The agents that select this Context, by `agent_id`, sorted and deduplicated.
async fn context_agents(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    context_id: &str,
) -> Result<Vec<String>> {
    let query = format!(
        "{{ Agent(filter: {{node_did: {{_eq: \"{}\"}}, context_id: {{_eq: \"{}\"}}}}) {{agent_id}} }}",
        escape_graphql_string(owner),
        escape_graphql_string(context_id)
    );
    let response = txn.execute(&query).await?;
    let rows = response["data"]["Agent"]
        .as_array()
        .cloned()
        .context("Agent reference query missing rows")?;
    let mut agents = Vec::new();
    for row in &rows {
        agents.push(
            row["agent_id"]
                .as_str()
                .context("referring agent ID missing")?
                .to_owned(),
        );
    }
    agents.sort();
    agents.dedup();
    Ok(agents)
}

/// Every agent that selects this Context or Tools, by `agent_id`, sorted and
/// deduplicated. Empty for targets that are not shared by reference.
///
/// A Context is shared when more than one Agent selects it; Tools are shared
/// when more than one Agent reaches them, directly or through a Context.
pub(crate) async fn shared_referrers(
    txn: &ConfigApplyTxn<'_>,
    target: SelfConfigTarget,
    owner: &str,
    id: &str,
) -> Result<Vec<String>> {
    match target {
        SelfConfigTarget::AgentContext => context_agents(txn, owner, id).await,
        SelfConfigTarget::Tools => {
            let query = format!(
                "{{ AgentContext(filter: {{node_did: {{_eq: \"{}\"}}, tools_id: {{_eq: \"{}\"}}}}) {{context_id}} }}",
                escape_graphql_string(owner),
                escape_graphql_string(id)
            );
            let response = txn.execute(&query).await?;
            let rows = response["data"]["AgentContext"]
                .as_array()
                .cloned()
                .context("AgentContext reference query missing rows")?;
            let mut agents = Vec::new();
            for row in &rows {
                let context_id = row["context_id"]
                    .as_str()
                    .context("referring context ID missing")?;
                agents.extend(context_agents(txn, owner, context_id).await?);
            }
            agents.sort();
            agents.dedup();
            Ok(agents)
        }
        _ => Ok(Vec::new()),
    }
}

/// The refusal for a shared Context or Tools, naming the option that accepts it.
pub(crate) fn shared_refusal(agents: &[String]) -> anyhow::Error {
    anyhow!(
        "targeted configuration requires an unshared Context and Tools; clone the working agent before editing shared configuration, or pass options.shared = \"acknowledge\" to edit it for every agent that selects it (agents: {})",
        agents.join(", ")
    )
}

/// The receipt for a shared edit: every agent the write reaches and the
/// sha256 of the document that was published. Absent when the document is not
/// shared, so an ordinary edit's receipt is unchanged.
///
/// `agents` is exactly the set the write modifies: the agents that select this
/// Context, or that reach this Tools directly or through a Context. It is not
/// widened to agents that merely use Tools this document newly selects — those
/// agents' own rows are untouched.
pub(super) async fn shared_receipt(
    txn: &ConfigApplyTxn<'_>,
    target: SelfConfigTarget,
    owner: &str,
    id: &str,
    merged: &Map<String, Value>,
) -> Result<Option<Value>> {
    let agents = shared_referrers(txn, target, owner, id).await?;
    if agents.len() <= 1 {
        return Ok(None);
    }
    use sha2::{Digest, Sha256};
    let bytes = canonical_json_bytes(&Value::Object(merged.clone()))?;
    Ok(Some(json!({
        "agents": agents,
        "sha256": format!("{:x}", Sha256::digest(&bytes)),
    })))
}

pub(super) fn replacement_plan(
    target: SelfConfigTarget,
    merged: &Map<String, Value>,
) -> Result<DesiredStateApplyPlan> {
    let value = Value::Object(merged.clone());
    DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: target.collection(),
        add: value.clone(),
        update: value,
    }])
}

pub(super) async fn candidate_doc(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    wanted: SelfConfigTarget,
    id: &str,
    target: SelfConfigTarget,
    merged: &Map<String, Value>,
) -> Result<Map<String, Value>> {
    if wanted == target && merged.get(target.unique_field()).and_then(Value::as_str) == Some(id) {
        return Ok(merged.clone());
    }
    Ok(read_owned_doc(txn, wanted, owner, id)
        .await?
        .with_context(|| format!("candidate chain references missing {} {id:?}; inspect the role with agent get, then select an existing document or create this dependency before selecting it", wanted.collection_name()))?
        .1)
}
/// Receipts summarize longer text fields by length and SHA-256: the caller
/// already holds the value it sent, and a prompt echoed in full costs its
/// size again in the model's context.
const RECEIPT_TEXT_LIMIT: usize = 2048;

pub(super) fn safe_diff(
    target: SelfConfigTarget,
    before: &Map<String, Value>,
    after: &Map<String, Value>,
) -> Vec<FieldDelta> {
    let mut deltas = diff_docs(target, before, after);
    for delta in &mut deltas {
        for value in [&mut delta.from, &mut delta.to] {
            if let Some(text) = value
                .as_str()
                .filter(|text| text.len() > RECEIPT_TEXT_LIMIT)
            {
                use sha2::{Digest, Sha256};
                *value = json!({"chars": text.chars().count(), "sha256": format!("{:x}", Sha256::digest(text))});
            }
        }
    }
    if target == SelfConfigTarget::InferenceBackend {
        for delta in &mut deltas {
            if delta.field == "auth" {
                for value in [&mut delta.from, &mut delta.to] {
                    if let Some(auth) = value.as_object_mut() {
                        if auth.contains_key("key") {
                            auth.insert("key".into(), Value::String("[redacted]".into()));
                        }
                    }
                }
            }
        }
    }
    deltas
}

/// The refusal when the caller acknowledges a shared edit but holds no
/// authority over every agent the write reaches.
///
/// `options.shared = "acknowledge"` waives only the sharing refusal. It never
/// widens who may write: a caller that may edit its own Context or Tools still
/// needs the agent catalog grant (category `node`) to change one that other
/// agents select, because the write lands on their configuration too.
pub(crate) fn shared_authority_refusal(agents: &[String]) -> anyhow::Error {
    anyhow!(
        "options.shared = \"acknowledge\" waives only the sharing refusal; editing a Context or Tools that other agents select requires the agent catalog grant (category node), because the write changes their configuration too (agents: {}). Clone the working agent to customize it independently, or ask the operator for the node grant",
        agents.join(", ")
    )
}

/// Enforce the caller's authority over every agent a shared write reaches.
///
/// Runs inside the same transaction as validation and publication, so the
/// referrer list cannot go stale between the check and the write. The
/// acknowledgement is a waiver of the sharing refusal only: with more than one
/// referrer it requires the agent catalog grant, and it never borrows a
/// sibling's authority — the anchor is the invoking agent, not whichever
/// referrer sorts first.
pub(crate) async fn guard_shared_authority_in_txn(
    txn: &ConfigApplyTxn<'_>,
    target: SelfConfigTarget,
    owner: &str,
    id: &str,
    acknowledged: bool,
    holds_catalog_grant: bool,
) -> Result<Vec<String>> {
    let agents = shared_referrers(txn, target, owner, id).await?;
    if agents.len() <= 1 {
        return Ok(agents);
    }
    if !acknowledged {
        return Err(shared_refusal(&agents));
    }
    anyhow::ensure!(holds_catalog_grant, "{}", shared_authority_refusal(&agents));
    Ok(agents)
}

/// Serialize a JSON value with object keys sorted at every depth, so the
/// receipt's sha256 identifies the published document's content and not the
/// order a map happened to iterate in.
fn canonical_json_bytes(value: &Value) -> Result<Vec<u8>> {
    fn canonical(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut sorted = Map::new();
                let mut keys = map.keys().collect::<Vec<_>>();
                keys.sort();
                for key in keys {
                    sorted.insert(key.clone(), canonical(&map[key]));
                }
                Value::Object(sorted)
            }
            Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
            other => other.clone(),
        }
    }
    Ok(serde_json::to_vec(&canonical(value))?)
}
