//! Native `{tool_id}_query` tool. Allowlisted JSON-RPC reads only.

use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::llm::tool::{BoxFuture, ToolDefinition, ToolDyn, ToolError};
use crate::tool_call_lifecycle::FailureClass;

use super::methods::validate_query_methods;
use super::rpc::HttpEthRpc;

/// Expanded, runtime-ready query surface for one EthTool document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEthQuery {
    pub tool_id: String,
    pub chain_id: u64,
    pub rpc_url: String,
    pub methods: Vec<String>,
}

impl ResolvedEthQuery {
    pub fn tool_name(&self) -> String {
        format!("{}_query", self.tool_id)
    }

    pub fn from_document(doc: &crate::document_config::EthToolDocument) -> Result<Option<Self>> {
        if !doc.enabled {
            return Ok(None);
        }
        let methods = validate_query_methods(doc.query_methods.as_deref().unwrap_or(&[]))?;
        if methods.is_empty() {
            return Ok(None);
        }
        let rpc_url = doc
            .rpc_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("EthTool {} has an empty rpc_url", doc.tool_id))?
            .to_string();
        let chain_id = doc
            .chain_id
            .ok_or_else(|| anyhow!("EthTool {} has no chain_id", doc.tool_id))?;
        if chain_id <= 0 {
            anyhow::bail!("EthTool {} chain_id must be positive", doc.tool_id);
        }
        Ok(Some(Self {
            tool_id: doc.tool_id.clone(),
            chain_id: chain_id as u64,
            rpc_url,
            methods,
        }))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EthQueryArgs {
    method: String,
    #[serde(default)]
    params: Value,
}

pub struct EthQueryTool {
    resolved: ResolvedEthQuery,
}

impl EthQueryTool {
    pub fn new(resolved: ResolvedEthQuery) -> Self {
        Self { resolved }
    }
}

impl ToolDyn for EthQueryTool {
    fn name(&self) -> String {
        self.resolved.tool_name()
    }

    fn definition(&self, _prompt: String) -> BoxFuture<'_, ToolDefinition> {
        let name = self.resolved.tool_name();
        let methods = self.resolved.methods.clone();
        Box::pin(async move {
            ToolDefinition {
                name,
                description: "Call an allowlisted read-only Ethereum JSON-RPC method. \
                     Params is a JSON array. Block tags default to \"latest\". \
                     Unfiltered eth_getLogs is rejected."
                    .to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "method": {
                            "type": "string",
                            "enum": methods,
                            "description": "JSON-RPC method from the EthTool query_methods allowlist."
                        },
                        "params": {
                            "type": "array",
                            "description": "JSON-RPC params array. Default: []"
                        }
                    },
                    "required": ["method"]
                }),
            }
        })
    }

    fn call(&self, args: String) -> BoxFuture<'_, Result<String, ToolError>> {
        let resolved = self.resolved.clone();
        Box::pin(async move {
            let parsed: EthQueryArgs = crate::llm::tool::parse_tool_args(&args)?;
            let params = match parsed.params {
                Value::Null => json!([]),
                Value::Array(_) => parsed.params,
                other => {
                    return Err(ToolError::ReportedFailure {
                        class: FailureClass::PolicyDenied,
                        text: format!("eth_query params must be a JSON array, got {other}"),
                    });
                }
            };
            let client = HttpEthRpc::http(&resolved.rpc_url, resolved.chain_id, &resolved.methods)
                .map_err(|error| ToolError::ReportedFailure {
                    class: FailureClass::Transport,
                    text: error.to_string(),
                })?;
            let result = client.call(&parsed.method, params).await.map_err(|error| {
                let text = error.to_string();
                let class = if text.contains("not in the configured query_methods")
                    || text.contains("not a read-only")
                    || text.contains("unfiltered")
                    || text.contains("exceeds max")
                {
                    FailureClass::PolicyDenied
                } else if text.contains("pruned-history") {
                    FailureClass::External
                } else {
                    FailureClass::Transport
                };
                ToolError::ReportedFailure { class, text }
            })?;
            serde_json::to_string(&result).map_err(ToolError::JsonError)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document_config::EthToolDocument;

    fn doc(enabled: bool, methods: &[&str]) -> EthToolDocument {
        EthToolDocument {
            tool_id: "base-read".to_string(),
            agent_did: "did:key:zAlice".to_string(),
            display_name: Some("base".to_string()),
            enabled,
            chain_id: Some(8453),
            rpc_url: Some("https://mainnet.base.org".to_string()),
            query_methods: Some(methods.iter().map(|m| m.to_string()).collect()),
            calls: None,
            key_binding_id: None,
            created_at: None,
        }
    }

    #[test]
    fn disabled_or_empty_methods_do_not_advertise() {
        assert!(
            ResolvedEthQuery::from_document(&doc(false, &["eth_chainId"]))
                .expect("ok")
                .is_none()
        );
        assert!(ResolvedEthQuery::from_document(&doc(true, &[]))
            .expect("ok")
            .is_none());
    }

    #[test]
    fn enabled_with_methods_names_query_tool() {
        let resolved =
            ResolvedEthQuery::from_document(&doc(true, &["eth_chainId", "eth_blockNumber"]))
                .expect("ok")
                .expect("advertised");
        assert_eq!(resolved.tool_name(), "base-read_query");
        assert_eq!(resolved.methods, vec!["eth_chainId", "eth_blockNumber"]);
    }

    #[test]
    fn send_method_fails_resolve() {
        let err = ResolvedEthQuery::from_document(&doc(true, &["eth_sendRawTransaction"]))
            .expect_err("send");
        assert!(err.to_string().contains("not a read-only"));
    }

    #[tokio::test]
    async fn definition_has_method_enum_and_no_data_field() {
        let resolved = ResolvedEthQuery::from_document(&doc(true, &["eth_chainId"]))
            .expect("ok")
            .expect("advertised");
        let tool = EthQueryTool::new(resolved);
        let definition = tool.definition(String::new()).await;
        assert_eq!(definition.name, "base-read_query");
        let params = &definition.parameters;
        assert_eq!(
            params["properties"]["method"]["enum"],
            json!(["eth_chainId"])
        );
        assert!(params["properties"].get("data").is_none());
        assert!(params["properties"].get("calldata").is_none());
    }
}
