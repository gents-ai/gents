use anyhow::{bail, Context, Result};
use gents::config_client::ConfigAccess;
use gents::{eth_tool_by_id_query, EthToolDocument, HttpEthRpc};
use serde_json::{json, Value};

use crate::cli::args::ChainQueryArgs;
use crate::{print_json, resolve_config_access, resolve_node_did};

#[derive(Debug, thiserror::Error)]
pub(crate) enum ChainQueryError {
    #[error("parsing RPC params JSON")]
    ParamsJson(#[source] serde_json::Error),
    #[error("RPC params must be a JSON array")]
    ParamsNotArray,
    #[error("response missing EthTool rows")]
    MissingRows,
    #[error("EthTool rows must be an array")]
    RowsNotArray,
    #[error("EthTool row must be an object")]
    RowNotObject,
    #[error("decoding EthTool rows")]
    DecodeRows(#[source] serde_json::Error),
}

pub(crate) async fn dispatch(args: ChainQueryArgs) -> Result<()> {
    let principal = resolve_node_did(args.access.home.as_deref(), None)?;
    let (access, _) =
        resolve_config_access(args.access.home.as_deref(), args.access.graphql.as_deref()).await?;
    let doc = load_eth_tool(&access, &principal, &args.tool_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("EthTool {:?} not found", args.tool_id))?;
    if doc.node_did != principal {
        bail!("EthTool is not owned by the local node");
    }
    if !doc.enabled {
        bail!("EthTool {:?} is disabled", args.tool_id);
    }
    let rpc_url = doc
        .rpc_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("EthTool {:?} has an empty rpc_url", args.tool_id))?;
    let chain_id = doc
        .chain_id
        .ok_or_else(|| anyhow::anyhow!("EthTool {:?} has no chain_id", args.tool_id))?;
    if chain_id <= 0 {
        bail!("EthTool {:?} chain_id must be positive", args.tool_id);
    }
    let methods = doc.query_methods.clone().unwrap_or_default();
    if methods.is_empty() {
        bail!(
            "EthTool {:?} query_methods is empty; eth_query is denied",
            args.tool_id
        );
    }
    let params = parse_params(args.params.as_deref())?;
    let client = HttpEthRpc::http_with_timeout(
        rpc_url,
        chain_id as u64,
        &methods,
        HttpEthRpc::configured_timeout(doc.rpc_timeout_secs)?,
    )?;
    let result = client
        .call(&args.method, params)
        .await
        .with_context(|| format!("calling {} on {}", args.method, args.tool_id))?;
    Ok(print_json(&json!({
        "tool_id": args.tool_id,
        "method": args.method,
        "result": result,
    }))?)
}

fn parse_params(raw: Option<&str>) -> std::result::Result<Value, ChainQueryError> {
    match raw.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(json!([])),
        Some(text) => {
            let value: Value = serde_json::from_str(text).map_err(ChainQueryError::ParamsJson)?;
            if !value.is_array() {
                return Err(ChainQueryError::ParamsNotArray);
            }
            Ok(value)
        }
    }
}

async fn load_eth_tool(
    access: &ConfigAccess,
    node_did: &str,
    tool_id: &str,
) -> Result<Option<EthToolDocument>> {
    let mut rows = decode_eth_tool_rows(
        &access
            .execute(&eth_tool_by_id_query(node_did, tool_id)?)
            .await?,
    )?;
    anyhow::ensure!(rows.len() <= 1, "duplicate Ethereum tool within node scope");
    anyhow::ensure!(
        rows.iter()
            .all(|row| row.node_did == node_did && row.tool_id == tool_id),
        "Ethereum lookup returned mismatched scoped identity"
    );
    Ok(rows.pop())
}

fn decode_eth_tool_rows(
    value: &Value,
) -> std::result::Result<Vec<EthToolDocument>, ChainQueryError> {
    let rows = value
        .pointer("/data/EthTool")
        .or_else(|| value.get("EthTool"))
        .cloned()
        .ok_or(ChainQueryError::MissingRows)?;
    if rows.is_null() {
        return Ok(Vec::new());
    }
    let mut rows = rows
        .as_array()
        .ok_or(ChainQueryError::RowsNotArray)?
        .clone();
    for row in &mut rows {
        row.as_object_mut()
            .ok_or(ChainQueryError::RowNotObject)?
            .remove("_docID");
    }
    serde_json::from_value(Value::Array(rows)).map_err(ChainQueryError::DecodeRows)
}
