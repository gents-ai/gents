use anyhow::{bail, Context, Result};
use gents::config_client::ConfigAccess;
use gents::{eth_tool_by_id_query, EthToolDocument, HttpEthRpc};
use serde_json::{json, Value};

use crate::cli::args::ChainQueryArgs;
use crate::{print_json, resolve_agent_did, resolve_config_access};

pub(crate) async fn dispatch(args: ChainQueryArgs) -> Result<()> {
    let principal = resolve_agent_did(args.access.home.as_deref(), None)?;
    let (access, _) =
        resolve_config_access(args.access.home.as_deref(), args.access.graphql.as_deref()).await?;
    let doc = load_eth_tool(&access, &args.tool_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("EthTool {:?} not found", args.tool_id))?;
    if doc.agent_did != principal {
        bail!("EthTool is not owned by the local principal");
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
    let client = HttpEthRpc::http(rpc_url, chain_id as u64, &methods)?;
    let result = client
        .call(&args.method, params)
        .await
        .with_context(|| format!("calling {} on {}", args.method, args.tool_id))?;
    print_json(&json!({
        "tool_id": args.tool_id,
        "method": args.method,
        "result": result,
    }))
}

fn parse_params(raw: Option<&str>) -> Result<Value> {
    match raw.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(json!([])),
        Some(text) => {
            let value: Value = serde_json::from_str(text).context("parsing RPC params JSON")?;
            if !value.is_array() {
                bail!("RPC params must be a JSON array");
            }
            Ok(value)
        }
    }
}

async fn load_eth_tool(access: &ConfigAccess, tool_id: &str) -> Result<Option<EthToolDocument>> {
    decode_eth_tool_rows(&access.execute(&eth_tool_by_id_query(tool_id)).await?)
        .map(|rows| rows.into_iter().next())
}

fn decode_eth_tool_rows(value: &Value) -> Result<Vec<EthToolDocument>> {
    let rows = value
        .pointer("/data/EthTool")
        .or_else(|| value.get("EthTool"))
        .cloned()
        .unwrap_or(Value::Array(Vec::new()));
    if rows.is_null() {
        return Ok(Vec::new());
    }
    serde_json::from_value(rows).context("decoding EthTool rows")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_params_defaults_to_empty_array() {
        assert_eq!(parse_params(None).expect("none"), json!([]));
        assert_eq!(parse_params(Some("")).expect("empty"), json!([]));
        assert_eq!(parse_params(Some("[]")).expect("arr"), json!([]));
        assert_eq!(
            parse_params(Some(r#"["0x1"]"#)).expect("one"),
            json!(["0x1"])
        );
        assert!(parse_params(Some(r#"{"to":"0x"}"#)).is_err());
    }
}
