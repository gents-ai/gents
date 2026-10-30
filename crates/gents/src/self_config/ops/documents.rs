use anyhow::{anyhow, Context, Result};
use serde_json::{json, Map, Value};

use crate::config_client::patch::{diff_docs, FieldDelta, SelfConfigTarget};
use crate::config_client::{
    read_desired_state_record_in_txn, ConfigApplyTxn, DesiredStateApplyDocument,
    DesiredStateApplyPlan,
};
use crate::document_config::Tools;
use crate::toolset::CommandNetworkMode;

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
        match super::super::command::field_shape(collection, &path) {
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
        crate::toolset::lsp::LspConfigDocument::parse_self_config(lsp.config.as_deref())
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
