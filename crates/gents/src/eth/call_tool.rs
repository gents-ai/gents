//! Generated read and `any_read` tools. ABI declarations are compiled once
//! when the runtime snapshot is built, then reused for schema and execution.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use alloy_dyn_abi::{DynSolValue, FunctionExt, JsonAbiExt};
use alloy_json_abi::Function;
use alloy_primitives::{keccak256, Address, I256, U256};
use anyhow::{anyhow, bail, Context, Result};
use k256::elliptic_curve::zeroize::Zeroizing;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::defra_node::EmbeddedNode;
use crate::document_config::{load_chain_key_binding, load_eth_tool};
use crate::llm::tool::{BoxFuture, ToolDefinition, ToolDyn, ToolError};
use crate::tool_call_lifecycle::FailureClass;

use super::calls::{
    compile_params, native_transfer_function, parse_abi_function, parse_u128, require_address,
    CallDecl, CompiledParam,
};
use super::keys::{
    address_from_secret, attestation_payload, binding_storage_key, decode_attestation,
    ChainKeyMaterialStore, KeyringChainKeyStore, KEY_BACKEND_KEYRING,
};
use super::rpc::HttpEthRpc;
use super::submit::{
    global_nonce_gate, submit_transaction, GasCaps, SubmitOptions, SubmitRequest, SubmitStatus,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEthCall {
    pub(crate) eth_tool_id: String,
    pub(crate) tool_name: String,
    pub(crate) chain_id: u64,
    pub(crate) rpc_url: String,
    pub(crate) description: String,
    pub(crate) kind: ResolvedCallKind,
    pub(crate) principal_did: String,
    pub(crate) binding_id: Option<String>,
    pub(crate) caps: GasCaps,
}

impl ResolvedEthCall {
    pub(crate) fn is_signing(&self) -> bool {
        matches!(self.kind, ResolvedCallKind::Write { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResolvedCallKind {
    AnyRead,
    Declared {
        to: String,
        function: Function,
        params: Vec<CompiledParam>,
    },
    Write {
        to: Option<String>,
        function: Option<Function>,
        params: Vec<CompiledParam>,
    },
}

impl ResolvedEthCall {
    pub(crate) fn from_decls(
        tool_id: &str,
        chain_id: u64,
        rpc_url: &str,
        decls: &[CallDecl],
        principal_did: &str,
        binding_id: Option<&str>,
    ) -> Result<Vec<Self>> {
        let mut out = Vec::new();
        let binding_id = binding_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        for decl in decls {
            match decl {
                CallDecl::AnyRead => out.push(Self {
                    eth_tool_id: tool_id.to_string(),
                    tool_name: format!("{tool_id}_any_read"),
                    chain_id,
                    rpc_url: rpc_url.to_string(),
                    description: "ABI-encoded primitive eth_call. Supply a signature and arguments; raw calldata is never accepted."
                        .to_string(),
                    kind: ResolvedCallKind::AnyRead,
                    principal_did: principal_did.to_string(),
                    binding_id: None,
                    caps: GasCaps::default(),
                }),
                CallDecl::Read {
                    tool_name,
                    to,
                    signature,
                    params,
                    description,
                } => {
                    let function = parse_abi_function(signature)?;
                    let params = compile_params(&function, params, false)?;
                    out.push(Self {
                        eth_tool_id: tool_id.to_string(),
                        tool_name: tool_name.clone(),
                        chain_id,
                        rpc_url: rpc_url.to_string(),
                        description: description
                            .clone()
                            .unwrap_or_else(|| format!("eth_call {signature} on {to}")),
                        kind: ResolvedCallKind::Declared {
                            to: to.clone(),
                            function,
                            params,
                        },
                        principal_did: principal_did.to_string(),
                        binding_id: None,
                        caps: GasCaps::default(),
                    });
                }
                CallDecl::Write {
                    tool_name,
                    to,
                    signature,
                    params,
                    description,
                    max_gas,
                    max_fee_per_gas,
                } => {
                    let Some(binding_id) = binding_id.clone() else {
                        bail!("write tool {tool_name} requires a chain key binding");
                    };
                    let function = parse_abi_function(signature)?;
                    let params = compile_params(&function, params, true)?;
                    out.push(Self {
                        eth_tool_id: tool_id.to_string(),
                        tool_name: tool_name.clone(),
                        chain_id,
                        rpc_url: rpc_url.to_string(),
                        description: description
                            .clone()
                            .unwrap_or_else(|| format!("send {signature} to {to}")),
                        kind: ResolvedCallKind::Write {
                            to: Some(to.clone()),
                            function: Some(function),
                            params,
                        },
                        principal_did: principal_did.to_string(),
                        binding_id: Some(binding_id),
                        caps: gas_caps(*max_gas, max_fee_per_gas.as_deref())?,
                    });
                }
                CallDecl::NativeTransfer {
                    tool_name,
                    params,
                    description,
                    max_gas,
                    max_fee_per_gas,
                } => {
                    let Some(binding_id) = binding_id.clone() else {
                        bail!("native_transfer {tool_name} requires a chain key binding");
                    };
                    let params = compile_params(&native_transfer_function()?, params, true)?;
                    out.push(Self {
                        eth_tool_id: tool_id.to_string(),
                        tool_name: tool_name.clone(),
                        chain_id,
                        rpc_url: rpc_url.to_string(),
                        description: description
                            .clone()
                            .unwrap_or_else(|| "send native value".to_string()),
                        kind: ResolvedCallKind::Write {
                            to: None,
                            function: None,
                            params,
                        },
                        principal_did: principal_did.to_string(),
                        binding_id: Some(binding_id),
                        caps: gas_caps(*max_gas, max_fee_per_gas.as_deref())?,
                    });
                }
            }
        }
        Ok(out)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AnyReadArgs {
    to: String,
    signature: String,
    #[serde(default)]
    args: Value,
    #[serde(default)]
    block: Option<String>,
}

pub(crate) struct EthCallTool {
    resolved: ResolvedEthCall,
    node: Arc<EmbeddedNode>,
}

impl EthCallTool {
    pub(crate) fn new(resolved: ResolvedEthCall, node: Arc<EmbeddedNode>) -> Self {
        Self { resolved, node }
    }
}

impl ToolDyn for EthCallTool {
    fn name(&self) -> String {
        self.resolved.tool_name.clone()
    }

    fn definition(&self, _prompt: String) -> BoxFuture<'_, ToolDefinition> {
        let resolved = self.resolved.clone();
        Box::pin(async move {
            ToolDefinition {
                name: resolved.tool_name,
                description: resolved.description,
                parameters: call_parameters(&resolved.kind),
            }
        })
    }

    fn call(&self, args: String) -> BoxFuture<'_, Result<String, ToolError>> {
        let resolved = self.resolved.clone();
        let node = self.node.clone();
        Box::pin(async move { execute_call(&node, &resolved, &args).await })
    }
}

fn call_parameters(kind: &ResolvedCallKind) -> Value {
    match kind {
        ResolvedCallKind::AnyRead => json!({
            "type": "object",
            "properties": {
                "to": { "type": "string", "description": "20-byte contract address" },
                "signature": { "type": "string", "description": "Primitive ABI signature, e.g. balanceOf(address)" },
                "args": { "type": "array", "description": "ABI arguments matching the signature" },
                "block": { "type": "string", "description": "Block tag. Default latest." }
            },
            "required": ["to", "signature"],
            "additionalProperties": false
        }),
        ResolvedCallKind::Declared { params, .. } | ResolvedCallKind::Write { params, .. } => {
            declared_schema(params)
        }
    }
}

fn declared_schema(params: &[CompiledParam]) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for param in params
        .iter()
        .filter(|param| param.decl.source.trim() == "model")
    {
        let mut schema = match param.solidity_type.as_str() {
            "bool" => json!({ "type": "boolean" }),
            ty if ty.starts_with("uint") || ty.starts_with("int") => json!({
                "anyOf": [{ "type": "string" }, { "type": "integer" }]
            }),
            _ => json!({ "type": "string" }),
        };
        if let Some(values) = &param.decl.enum_values {
            schema["enum"] = json!(values);
        }
        properties.insert(param.name.clone(), schema);
        required.push(Value::String(param.name.clone()));
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

async fn execute_call(
    node: &EmbeddedNode,
    resolved: &ResolvedEthCall,
    args: &str,
) -> Result<String, ToolError> {
    let client = HttpEthRpc::http(&resolved.rpc_url, resolved.chain_id, &[])
        .map_err(|error| reported(FailureClass::Transport, error.to_string()))?;
    match &resolved.kind {
        ResolvedCallKind::AnyRead => {
            let parsed: AnyReadArgs = crate::llm::tool::parse_tool_args(args)?;
            require_address(&parsed.to, "any_read.to")
                .map_err(|error| reported(FailureClass::ArgumentInvalid, error.to_string()))?;
            let arg_values = match parsed.args {
                Value::Null => Vec::new(),
                Value::Array(items) => items,
                other => {
                    return Err(reported(
                        FailureClass::ArgumentInvalid,
                        format!("any_read args must be a JSON array, got {other}"),
                    ));
                }
            };
            let function = parse_abi_function(&parsed.signature)
                .map_err(|error| reported(FailureClass::ArgumentInvalid, error.to_string()))?;
            let data = encode_function(&function, &arg_values)
                .map_err(|error| reported(FailureClass::ArgumentInvalid, error.to_string()))?;
            let result = client
                .eth_call(&parsed.to, &data, parsed.block.as_deref())
                .await
                .map_err(|error| reported(FailureClass::Transport, error.to_string()))?;
            decode_or_hex(&function, &result)
        }
        ResolvedCallKind::Declared {
            to,
            function,
            params,
        } => {
            let parsed: BTreeMap<String, Value> = crate::llm::tool::parse_tool_args(args)?;
            let arg_values = bind_params(params, &parsed, None)
                .map_err(|error| reported(FailureClass::PolicyDenied, error.to_string()))?;
            let data = encode_function(function, &arg_values)
                .map_err(|error| reported(FailureClass::ArgumentInvalid, error.to_string()))?;
            let result = client
                .eth_call(to, &data, None)
                .await
                .map_err(|error| reported(FailureClass::Transport, error.to_string()))?;
            decode_or_hex(function, &result)
        }
        ResolvedCallKind::Write {
            to,
            function,
            params,
        } => {
            execute_write(
                node,
                resolved,
                &client,
                to.as_deref(),
                function.as_ref(),
                params,
                args,
            )
            .await
        }
    }
}

async fn execute_write(
    node: &EmbeddedNode,
    resolved: &ResolvedEthCall,
    client: &HttpEthRpc,
    to: Option<&str>,
    function: Option<&Function>,
    params: &[CompiledParam],
    args: &str,
) -> Result<String, ToolError> {
    let runtime =
        crate::tool_call_lifecycle::runtime::current_tool_runtime_context().ok_or_else(|| {
            reported(
                FailureClass::PolicyDenied,
                "eth write has no runtime identity".to_string(),
            )
        })?;
    if runtime.agent_did.as_deref() != Some(resolved.principal_did.as_str()) {
        return Err(reported(
            FailureClass::PolicyDenied,
            "eth write runtime principal does not own its key binding".to_string(),
        ));
    }
    let parsed: BTreeMap<String, Value> = crate::llm::tool::parse_tool_args(args)?;
    let secret = Zeroizing::new(
        load_signing_key(node, resolved)
            .await
            .map_err(|error| reported(FailureClass::PolicyDenied, error.to_string()))?,
    );
    let from = address_from_secret(&secret)
        .map_err(|error| reported(FailureClass::PolicyDenied, error.to_string()))?;
    let bound = bind_params(params, &parsed, Some(&from))
        .map_err(|error| reported(FailureClass::PolicyDenied, error.to_string()))?;
    let (to, data, value) = if let Some(function) = function {
        let encoded = encode_function(function, &bound)
            .map_err(|error| reported(FailureClass::ArgumentInvalid, error.to_string()))?;
        let data = decode_hex(&encoded)
            .map_err(|error| reported(FailureClass::ArgumentInvalid, error.to_string()))?;
        (to.map(ToOwned::to_owned), data, U256::ZERO)
    } else {
        let destination = bound.first().and_then(Value::as_str).ok_or_else(|| {
            reported(
                FailureClass::ArgumentInvalid,
                "native_transfer to must be an address".to_string(),
            )
        })?;
        let value = bound
            .get(1)
            .ok_or_else(|| {
                reported(
                    FailureClass::ArgumentInvalid,
                    "native_transfer value is missing".to_string(),
                )
            })
            .and_then(|value| {
                parse_uint(value)
                    .map_err(|error| reported(FailureClass::ArgumentInvalid, error.to_string()))
            })?;
        (Some(destination.to_string()), Vec::new(), value)
    };
    let request_id = runtime
        .request_id
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            reported(
                FailureClass::PolicyDenied,
                "eth write requires a durable request id".to_string(),
            )
        })?;
    let idempotency_key = write_idempotency_key(
        &request_id,
        &resolved.tool_name,
        to.as_deref(),
        value,
        &data,
    )
    .map_err(ToolError::JsonError)?;
    let receipt = submit_transaction(
        node,
        client,
        &secret,
        SubmitRequest {
            principal_did: resolved.principal_did.clone(),
            chain_id: resolved.chain_id,
            from,
            to,
            value,
            data,
            caps: resolved.caps,
            idempotency_key,
        },
        global_nonce_gate(),
        SubmitOptions {
            receipt_attempts: 8,
            receipt_interval: std::time::Duration::from_millis(500),
        },
    )
    .await;
    let receipt = receipt.map_err(|error| reported(FailureClass::External, error.to_string()))?;
    if receipt.status == SubmitStatus::ConfirmedReverted {
        return Err(reported(
            FailureClass::External,
            format!("Ethereum transaction {} reverted", receipt.tx_hash),
        ));
    }
    serde_json::to_string(&json!({
        "tx_hash": receipt.tx_hash,
        "status": match receipt.status {
            SubmitStatus::ConfirmedSuccess => "confirmed_success",
            SubmitStatus::SubmittedUnknown => "submitted_unknown",
            SubmitStatus::ConfirmedReverted => unreachable!(),
        },
        "receipt": receipt.receipt,
    }))
    .map_err(ToolError::JsonError)
}

fn write_idempotency_key(
    request_id: &str,
    tool_name: &str,
    to: Option<&str>,
    value: U256,
    data: &[u8],
) -> serde_json::Result<String> {
    // Recovery may mint a fresh in-memory tool-call id. Bind durability to the
    // request and exact Ethereum action instead; intentionally repeated,
    // byte-identical transfers belong in separate agent requests.
    let semantic_action = serde_json::to_vec(&json!({
        "request_id": request_id,
        "tool_name": tool_name,
        "to": to.map(str::to_ascii_lowercase),
        "value": value.to_string(),
        "data": format!("0x{}", hex_encode(data)),
    }))?;
    Ok(format!("{}:{:#x}", request_id, keccak256(semantic_action)))
}

async fn load_signing_key(node: &EmbeddedNode, resolved: &ResolvedEthCall) -> Result<[u8; 32]> {
    let binding_id = resolved
        .binding_id
        .as_deref()
        .ok_or_else(|| anyhow!("write tool has no chain key binding"))?;
    let tool = load_eth_tool(node, &resolved.eth_tool_id)
        .await?
        .ok_or_else(|| anyhow!("EthTool {:?} no longer exists", resolved.eth_tool_id))?;
    if !tool.enabled {
        bail!("EthTool {:?} is disabled", resolved.eth_tool_id);
    }
    if tool.agent_did != resolved.principal_did {
        bail!(
            "EthTool {:?} belongs to another principal",
            resolved.eth_tool_id
        );
    }
    if tool.chain_id != i64::try_from(resolved.chain_id).ok() {
        bail!("EthTool {:?} changed chain_id", resolved.eth_tool_id);
    }
    if tool.key_binding_id.as_deref() != Some(binding_id) {
        bail!(
            "EthTool {:?} no longer selects chain key binding {binding_id:?}",
            resolved.eth_tool_id
        );
    }
    let binding = load_chain_key_binding(node, binding_id)
        .await?
        .ok_or_else(|| anyhow!("chain key binding {binding_id:?} does not exist"))?;
    if binding.principal_did != resolved.principal_did {
        bail!("chain key binding {binding_id:?} belongs to another principal");
    }
    if binding
        .revoked_at
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        bail!("chain key binding {binding_id:?} is revoked");
    }
    if binding.key_backend.as_deref() != Some(KEY_BACKEND_KEYRING) {
        bail!("chain key binding {binding_id:?} has an unsupported backend");
    }
    let created_at = binding
        .created_at
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("chain key binding {binding_id:?} has no creation time"))?;
    let attestation = decode_attestation(
        binding
            .attestation
            .as_deref()
            .ok_or_else(|| anyhow!("chain key binding {binding_id:?} has no attestation"))?,
    )?;
    let payload = attestation_payload(
        binding_id,
        &binding.principal_did,
        &binding.address,
        KEY_BACKEND_KEYRING,
        created_at,
    );
    if !crate::identity::verify_did_signature(&binding.principal_did, &payload, &attestation)? {
        bail!("chain key binding {binding_id:?} has an invalid attestation");
    }
    let secret =
        KeyringChainKeyStore.load(&binding_storage_key(&binding.principal_did, binding_id))?;
    let address = address_from_secret(&secret)?;
    if !address.eq_ignore_ascii_case(&binding.address) {
        bail!("chain key material does not match binding {binding_id:?}");
    }
    Ok(secret)
}

fn reported(class: FailureClass, text: String) -> ToolError {
    ToolError::ReportedFailure { class, text }
}

fn bind_params(
    params: &[CompiledParam],
    model: &BTreeMap<String, Value>,
    self_address: Option<&str>,
) -> Result<Vec<Value>> {
    let model_names: BTreeSet<&str> = params
        .iter()
        .filter(|param| param.decl.source.trim() == "model")
        .map(|param| param.name.as_str())
        .collect();
    if let Some(extra) = model
        .keys()
        .find(|name| !model_names.contains(name.as_str()))
    {
        bail!("undeclared model param {extra}");
    }

    let mut out = Vec::with_capacity(params.len());
    for param in params {
        let value = match param.decl.source.trim() {
            "model" => model
                .get(&param.name)
                .cloned()
                .ok_or_else(|| anyhow!("missing model param {}", param.name))?,
            "fixed" => param.decl.value.clone().unwrap_or(Value::Null),
            "runtime" => {
                json!(self_address.ok_or_else(|| anyhow!("runtime self_address is unavailable"))?)
            }
            other => bail!("unsupported param source {other}"),
        };
        enforce_constraints(param, &value)?;
        out.push(value);
    }
    Ok(out)
}

fn enforce_constraints(param: &CompiledParam, value: &Value) -> Result<()> {
    if let Some(allowlist) = &param.decl.address_allowlist {
        let address = value
            .as_str()
            .ok_or_else(|| anyhow!("param {} address must be a string", param.name))?;
        if !allowlist
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(address))
        {
            bail!(
                "param {} address {} is not on the allowlist",
                param.name,
                address
            );
        }
    }
    if let Some(values) = &param.decl.enum_values {
        let actual = constraint_text(value)?;
        if !values.iter().any(|allowed| {
            if param.solidity_type == "address" {
                allowed.eq_ignore_ascii_case(&actual)
            } else {
                allowed == &actual
            }
        }) {
            bail!("param {} value {} is not in the enum", param.name, actual);
        }
    }
    if param.solidity_type.starts_with("uint") {
        let actual = parse_uint(value)?;
        if let Some(min) = &param.decl.min {
            if actual < parse_uint(&json!(min))? {
                bail!("param {} is below min {}", param.name, min);
            }
        }
        if let Some(max) = &param.decl.max {
            if actual > parse_uint(&json!(max))? {
                bail!("param {} exceeds max {}", param.name, max);
            }
        }
    } else if param.solidity_type.starts_with("int") {
        let actual = parse_int(value)?;
        if let Some(min) = &param.decl.min {
            if actual < parse_int(&json!(min))? {
                bail!("param {} is below min {}", param.name, min);
            }
        }
        if let Some(max) = &param.decl.max {
            if actual > parse_int(&json!(max))? {
                bail!("param {} exceeds max {}", param.name, max);
            }
        }
    }
    Ok(())
}

fn constraint_text(value: &Value) -> Result<String> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Number(value) => Ok(value.to_string()),
        Value::Bool(value) => Ok(value.to_string()),
        other => bail!("enum constraint does not support value {other}"),
    }
}

fn gas_caps(max_gas: Option<u64>, max_fee_per_gas: Option<&str>) -> Result<GasCaps> {
    let max_fee_per_gas = match max_fee_per_gas
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(text) => {
            Some(parse_u128(text).map_err(|error| anyhow!("max_fee_per_gas {text}: {error}"))?)
        }
        None => None,
    };
    Ok(GasCaps {
        max_gas,
        max_fee_per_gas,
    })
}

fn encode_function(function: &Function, args: &[Value]) -> Result<String> {
    if function.inputs.len() != args.len() {
        bail!(
            "signature {} expects {} args, got {}",
            function.signature(),
            function.inputs.len(),
            args.len()
        );
    }
    let mut values = Vec::with_capacity(args.len());
    for (param, value) in function.inputs.iter().zip(args) {
        values.push(
            json_to_dyn(&param.ty, value)
                .with_context(|| format!("encoding argument {} as {}", param.name, param.ty))?,
        );
    }
    let encoded = function
        .abi_encode_input(&values)
        .map_err(|error| anyhow!("ABI encode {}: {error}", function.signature()))?;
    Ok(format!("0x{}", hex_encode(&encoded)))
}

fn decode_or_hex(function: &Function, result: &Value) -> Result<String, ToolError> {
    let hex = result.as_str().ok_or_else(|| {
        reported(
            FailureClass::External,
            format!("eth_call result is not hex: {result}"),
        )
    })?;
    if function.outputs.is_empty() {
        return Ok(json!({ "data": hex }).to_string());
    }
    let bytes =
        decode_hex(hex).map_err(|error| reported(FailureClass::External, error.to_string()))?;
    let decoded = function
        .abi_decode_output(&bytes)
        .map_err(|error| reported(FailureClass::External, error.to_string()))?;
    let values: Vec<Value> = decoded.iter().map(dyn_to_json).collect();
    serde_json::to_string(&json!({ "data": hex, "decoded": values })).map_err(ToolError::JsonError)
}

fn json_to_dyn(ty: &str, value: &Value) -> Result<DynSolValue> {
    let ty = ty.trim();
    if ty == "address" {
        let text = value
            .as_str()
            .ok_or_else(|| anyhow!("address must be a string"))?;
        let address: Address = text
            .parse()
            .map_err(|error| anyhow!("address {text}: {error}"))?;
        return Ok(DynSolValue::Address(address));
    }
    if ty == "bool" {
        let flag = value
            .as_bool()
            .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
            .ok_or_else(|| anyhow!("bool expected"))?;
        return Ok(DynSolValue::Bool(flag));
    }
    if ty == "string" {
        let text = value.as_str().ok_or_else(|| anyhow!("string expected"))?;
        return Ok(DynSolValue::String(text.to_string()));
    }
    if let Some(size) = ty.strip_prefix("bytes").filter(|size| !size.is_empty()) {
        let size: usize = size
            .parse()
            .with_context(|| format!("invalid fixed bytes type {ty}"))?;
        if !(1..=32).contains(&size) {
            bail!("fixed bytes size must be between 1 and 32, got {size}");
        }
        let text = value
            .as_str()
            .ok_or_else(|| anyhow!("{ty} must be a hex string"))?;
        let bytes = decode_hex(text)?;
        if bytes.len() != size {
            bail!("{ty} requires exactly {size} bytes, got {}", bytes.len());
        }
        let mut padded = [0u8; 32];
        padded[..size].copy_from_slice(&bytes);
        return Ok(DynSolValue::FixedBytes(padded.into(), size));
    }
    if ty == "bytes" {
        let text = value
            .as_str()
            .ok_or_else(|| anyhow!("bytes must be a hex string"))?;
        return Ok(DynSolValue::Bytes(decode_hex(text)?));
    }
    if ty.starts_with("uint") {
        let bits = ty.trim_start_matches("uint").parse().unwrap_or(256);
        return Ok(DynSolValue::Uint(parse_uint(value)?, bits));
    }
    if ty.starts_with("int") {
        let bits = ty.trim_start_matches("int").parse().unwrap_or(256);
        return Ok(DynSolValue::Int(parse_int(value)?, bits));
    }
    bail!("unsupported Solidity type {ty}");
}

fn dyn_to_json(value: &DynSolValue) -> Value {
    match value {
        DynSolValue::Address(address) => json!(format!("{address}")),
        DynSolValue::Bool(flag) => json!(flag),
        DynSolValue::String(text) => json!(text),
        DynSolValue::Bytes(bytes) => json!(format!("0x{}", hex_encode(bytes))),
        DynSolValue::FixedBytes(bytes, size) => {
            json!(format!("0x{}", hex_encode(&bytes.as_slice()[..*size])))
        }
        DynSolValue::Uint(n, _) => json!(n.to_string()),
        DynSolValue::Int(n, _) => json!(n.to_string()),
        DynSolValue::Array(items) | DynSolValue::FixedArray(items) => {
            Value::Array(items.iter().map(dyn_to_json).collect())
        }
        DynSolValue::Tuple(items) => Value::Array(items.iter().map(dyn_to_json).collect()),
        other => json!(format!("{other:?}")),
    }
}

fn parse_uint(value: &Value) -> Result<U256> {
    match value {
        Value::String(text) => text
            .parse::<U256>()
            .or_else(|_| U256::from_str_radix(text.trim_start_matches("0x"), 16))
            .map_err(|error| anyhow!("uint {text}: {error}")),
        Value::Number(n) => {
            if n.as_f64().is_some_and(|f| f.fract() != 0.0) {
                bail!("uint must not be a float: {n}");
            }
            n.as_u64()
                .map(U256::from)
                .ok_or_else(|| anyhow!("uint {n} is not an integer"))
        }
        other => bail!("uint expected string or integer, got {other}"),
    }
}

fn parse_int(value: &Value) -> Result<I256> {
    match value {
        Value::String(text) => text
            .parse::<I256>()
            .map_err(|error| anyhow!("int {text}: {error}")),
        Value::Number(n) => n
            .as_i64()
            .map(I256::try_from)
            .and_then(Result::ok)
            .ok_or_else(|| anyhow!("int {n} is not an integer")),
        other => bail!("int expected string or integer, got {other}"),
    }
}

fn decode_hex(value: &str) -> Result<Vec<u8>> {
    let hex = value.strip_prefix("0x").unwrap_or(value).trim();
    if hex.len() % 2 != 0 {
        bail!("odd-length hex {value}");
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|error| anyhow!("{error}")))
        .collect()
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::calls::ParamDecl;
    use super::*;

    #[test]
    fn any_read_schema_has_no_data_field() {
        let resolved = ResolvedEthCall {
            eth_tool_id: "base".to_string(),
            tool_name: "base_any_read".to_string(),
            chain_id: 8453,
            rpc_url: "http://127.0.0.1".to_string(),
            description: "any".to_string(),
            kind: ResolvedCallKind::AnyRead,
            principal_did: "did:key:zAlice".to_string(),
            binding_id: None,
            caps: GasCaps::default(),
        };
        let parameters = call_parameters(&resolved.kind);
        assert!(parameters["properties"].get("data").is_none());
        assert!(parameters["properties"].get("calldata").is_none());
        assert_eq!(parameters["additionalProperties"], false);
    }

    #[test]
    fn write_idempotency_is_stable_per_request_and_semantic_action() {
        let key = |request_id| {
            write_idempotency_key(
                request_id,
                "base_transfer",
                Some("0x1111111111111111111111111111111111111111"),
                U256::from(7),
                &[],
            )
            .unwrap()
        };
        assert_eq!(key("request-1"), key("request-1"));
        assert_ne!(key("request-1"), key("request-2"));
    }

    #[test]
    fn write_tools_are_generated_when_keys_present() {
        let decls = crate::eth::parse_call_decls(Some(&[
            r#"{"kind":"write","tool_name":"usdc_transfer","to":"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913","signature":"transfer(address to,uint256 amount)","params":{"to":{"source":"model","address_allowlist":["0x1111111111111111111111111111111111111111"]},"amount":{"source":"model","max":"1000000"}},"max_gas":100000,"max_fee_per_gas":"2000000000"}"#.to_string(),
        ]))
        .unwrap();
        crate::eth::validate_call_decls(&decls).unwrap();
        let resolved = ResolvedEthCall::from_decls(
            "base",
            8453,
            "http://127.0.0.1",
            &decls,
            "did:key:zAlice",
            Some("bind-1"),
        )
        .unwrap();
        assert_eq!(resolved.len(), 1);
        assert!(resolved[0].is_signing());
        assert_eq!(resolved[0].tool_name, "usdc_transfer");
        let parameters = call_parameters(&resolved[0].kind);
        assert!(parameters["properties"].get("data").is_none());
        assert!(parameters["properties"].get("amount").is_some());
    }

    #[test]
    fn encode_balance_of_has_selector() {
        let function = parse_abi_function("balanceOf(address)").unwrap();
        let encoded = encode_function(
            &function,
            &[json!("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")],
        )
        .expect("encode");
        assert_eq!(&encoded[2..10], "70a08231");
    }

    #[test]
    fn fixed_bytes_are_right_padded() {
        let function = parse_abi_function("foo(bytes2)").unwrap();
        let encoded = encode_function(&function, &[json!("0x1234")]).expect("encode");
        assert_eq!(&encoded[10..14], "1234");
        assert!(encoded[14..].bytes().all(|byte| byte == b'0'));
    }

    #[test]
    fn binding_uses_abi_order_and_enforces_all_constraints() {
        let params = vec![
            CompiledParam {
                name: "recipient".to_string(),
                solidity_type: "address".to_string(),
                decl: ParamDecl {
                    address_allowlist: Some(vec![
                        "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".to_string()
                    ]),
                    ..ParamDecl::default()
                },
            },
            CompiledParam {
                name: "qty".to_string(),
                solidity_type: "uint256".to_string(),
                decl: ParamDecl {
                    min: Some("1".to_string()),
                    max: Some("10".to_string()),
                    enum_values: Some(vec!["5".to_string(), "6".to_string()]),
                    ..ParamDecl::default()
                },
            },
        ];
        let model = BTreeMap::from([
            ("qty".to_string(), json!(5)),
            (
                "recipient".to_string(),
                json!("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"),
            ),
        ]);
        let bound = bind_params(&params, &model, None).expect("bind");
        assert_eq!(bound[0], model["recipient"]);
        assert_eq!(bound[1], json!(5));

        let mut above_max = model.clone();
        above_max.insert("qty".to_string(), json!(11));
        assert!(bind_params(&params, &above_max, None).is_err());

        let mut outside_enum = model;
        outside_enum.insert("qty".to_string(), json!(7));
        assert!(bind_params(&params, &outside_enum, None).is_err());
    }
}
