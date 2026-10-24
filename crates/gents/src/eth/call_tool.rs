//! Generated EthTool call tools: reads, writes, native transfer, and EIP-712.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use alloy_dyn_abi::{DynSolValue, FunctionExt, JsonAbiExt};
use alloy_json_abi::Function;
use alloy_primitives::{keccak256, Address, I256, U256};
use anyhow::{anyhow, bail};
use k256::elliptic_curve::zeroize::Zeroizing;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::defra_node::EmbeddedNode;
use crate::document_config::{ChainKeyBindingDocument, EthToolDocument};
use crate::llm::tool::{BoxFuture, ToolDefinition, ToolDyn, ToolError};
use crate::tool_call_lifecycle::FailureClass;

use super::calls::{
    compile_params, compile_typed_params, native_transfer_function, parse_abi_function, parse_u128,
    require_address, typed_domain_chain_id, CallDecl, CompiledParam,
};
use super::error::{EthError, Result};
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
    pub(crate) rpc_timeout: std::time::Duration,
    pub(crate) description: String,
    pub(crate) kind: ResolvedCallKind,
    pub(crate) principal_did: String,
    pub(crate) binding_id: Option<String>,
    pub(crate) caps: GasCaps,
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
    SignTypedData {
        domain: Value,
        types: Value,
        primary_type: String,
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
        rpc_timeout: std::time::Duration,
    ) -> Result<Vec<Self>> {
        if rpc_timeout.is_zero() {
            return Err(EthError::ZeroTimeout);
        }
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
                    rpc_timeout,
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
                    rpc_timeout,
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
                        return Err(EthError::WriteRequiresBinding {
                            tool_name: tool_name.clone(),
                        });
                    };
                    let function = parse_abi_function(signature)?;
                    let params = compile_params(&function, params, true)?;
                    out.push(Self {
                        eth_tool_id: tool_id.to_string(),
                        tool_name: tool_name.clone(),
                        chain_id,
                        rpc_url: rpc_url.to_string(),
                    rpc_timeout,
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
                        return Err(EthError::NativeTransferRequiresBinding {
                            tool_name: tool_name.clone(),
                        });
                    };
                    let params = compile_params(&native_transfer_function()?, params, true)?;
                    out.push(Self {
                        eth_tool_id: tool_id.to_string(),
                        tool_name: tool_name.clone(),
                        chain_id,
                        rpc_url: rpc_url.to_string(),
                    rpc_timeout,
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
                CallDecl::SignTypedData {
                    tool_name,
                    domain,
                    types,
                    primary_type,
                    params,
                    description,
                } => {
                    let Some(binding_id) = binding_id.clone() else {
                        return Err(EthError::SignTypedRequiresBinding {
                            tool_name: tool_name.clone(),
                        });
                    };
                    match typed_domain_chain_id(domain)? {
                        Some(domain_chain_id) if domain_chain_id == chain_id => {}
                        Some(_) => {
                            return Err(EthError::SignTypedChainMismatch {
                                tool_name: tool_name.clone(),
                                chain_id,
                            })
                        }
                        None => {
                            return Err(EthError::SignTypedChainMissing {
                                tool_name: tool_name.clone(),
                                chain_id,
                            })
                        }
                    }
                    let params = compile_typed_params(types, primary_type, params, true)?;
                    out.push(Self {
                        eth_tool_id: tool_id.to_string(),
                        tool_name: tool_name.clone(),
                        chain_id,
                        rpc_url: rpc_url.to_string(),
                    rpc_timeout,
                        description: description.clone().unwrap_or_else(|| {
                            format!("sign EIP-712 {primary_type}; nothing is submitted")
                        }),
                        kind: ResolvedCallKind::SignTypedData {
                            domain: domain.clone(),
                            types: types.clone(),
                            primary_type: primary_type.clone(),
                            params,
                        },
                        principal_did: principal_did.to_string(),
                        binding_id: Some(binding_id),
                        caps: GasCaps::default(),
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
        ResolvedCallKind::Declared { params, .. }
        | ResolvedCallKind::Write { params, .. }
        | ResolvedCallKind::SignTypedData { params, .. } => declared_schema(params),
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
    node: &Arc<EmbeddedNode>,
    resolved: &ResolvedEthCall,
    args: &str,
) -> Result<String, ToolError> {
    let client = HttpEthRpc::http_with_timeout(
        &resolved.rpc_url,
        resolved.chain_id,
        &[],
        resolved.rpc_timeout,
    )
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
            for input in &function.inputs {
                if !super::calls::supported_primitive_type(&input.ty) {
                    return Err(reported(
                        FailureClass::ArgumentInvalid,
                        format!("any_read signature uses unsupported type {}", input.ty),
                    ));
                }
            }
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
        ResolvedCallKind::SignTypedData {
            domain,
            types,
            primary_type,
            params,
        } => {
            execute_sign_typed_data(node, resolved, domain, types, primary_type, params, args).await
        }
    }
}

async fn execute_sign_typed_data(
    node: &Arc<EmbeddedNode>,
    resolved: &ResolvedEthCall,
    domain: &Value,
    types: &Value,
    primary_type: &str,
    params: &[CompiledParam],
    args: &str,
) -> Result<String, ToolError> {
    require_runtime_principal(resolved)?;
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
    let mut message = serde_json::Map::new();
    for (index, param) in params.iter().enumerate() {
        if let Some(value) = bound.get(index) {
            message.insert(param.name.clone(), value.clone());
        }
    }
    let typed = json!({
        "types": types,
        "primaryType": primary_type,
        "domain": domain,
        "message": message,
    });
    let typed: alloy_dyn_abi::TypedData = serde_json::from_value(typed)
        .map_err(|error| reported(FailureClass::ArgumentInvalid, error.to_string()))?;
    let hash = typed
        .eip712_signing_hash()
        .map_err(|error| reported(FailureClass::ArgumentInvalid, error.to_string()))?;
    let mut digest = [0u8; 32];
    digest.copy_from_slice(hash.as_slice());
    let (r, s, y_parity) = crate::eth::sign_prehash_recoverable(&secret, &digest)
        .map_err(|error| reported(FailureClass::PolicyDenied, error.to_string()))?;
    let v = if y_parity { 28u8 } else { 27 };
    let mut sig = Vec::with_capacity(65);
    sig.extend_from_slice(&r);
    sig.extend_from_slice(&s);
    sig.push(v);
    serde_json::to_string(&json!({
        "address": from,
        "primary_type": primary_type,
        "digest": format!("0x{}", hex_encode(&digest)),
        "signature": format!("0x{}", hex_encode(&sig)),
        "submitted": false,
    }))
    .map_err(ToolError::JsonError)
}

async fn execute_write(
    node: &Arc<EmbeddedNode>,
    resolved: &ResolvedEthCall,
    client: &HttpEthRpc,
    to: Option<&str>,
    function: Option<&Function>,
    params: &[CompiledParam],
    args: &str,
) -> Result<String, ToolError> {
    require_runtime_principal(resolved)?;
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
    let runtime = crate::tool_call_lifecycle::runtime::current_tool_runtime_context()
        .expect("runtime node identity was checked before signing");
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
    let receipt = receipt.map_err(|error| {
        let message = error.to_string();
        let class = if super::submit::is_gas_or_fee_cap_error(error.as_ref()) {
            FailureClass::PolicyDenied
        } else {
            FailureClass::External
        };
        reported(class, message)
    })?;
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
    let semantic_action = serde_json::to_vec(&json!({
        "request_id": request_id,
        "tool_name": tool_name,
        "to": to.map(str::to_ascii_lowercase),
        "value": value.to_string(),
        "data": format!("0x{}", hex_encode(data)),
    }))?;
    Ok(format!("{}:{:#x}", request_id, keccak256(semantic_action)))
}

fn require_runtime_principal(resolved: &ResolvedEthCall) -> Result<(), ToolError> {
    let runtime =
        crate::tool_call_lifecycle::runtime::current_tool_runtime_context().ok_or_else(|| {
            reported(
                FailureClass::PolicyDenied,
                "Ethereum signing has no runtime identity".to_string(),
            )
        })?;
    if runtime.node_did.as_deref() != Some(resolved.principal_did.as_str()) {
        return Err(reported(
            FailureClass::PolicyDenied,
            "Ethereum signing runtime node does not own its key binding".to_string(),
        ));
    }
    Ok(())
}

async fn load_signing_key(
    node: &Arc<EmbeddedNode>,
    resolved: &ResolvedEthCall,
) -> anyhow::Result<[u8; 32]> {
    load_signing_key_with_store(node, resolved, &KeyringChainKeyStore).await
}

async fn load_signing_key_with_store(
    node: &std::sync::Arc<EmbeddedNode>,
    resolved: &ResolvedEthCall,
    store: &dyn ChainKeyMaterialStore,
) -> anyhow::Result<[u8; 32]> {
    let binding_id = resolved
        .binding_id
        .as_deref()
        .ok_or_else(|| anyhow!("write tool has no chain key binding"))?;
    let (tool, binding): (EthToolDocument, ChainKeyBindingDocument) =
        crate::config_client::ConfigAccess::transact_local_write(
            node,
            None,
            "eth.signing_config.read",
            |txn| {
                Box::pin(async move {
                    let tool = crate::config_client::read_desired_state_document_in_txn(
                        txn,
                        crate::Collection::EthTool,
                        &resolved.principal_did,
                        &resolved.eth_tool_id,
                    )
                    .await?
                    .ok_or_else(|| {
                        anyhow!("EthTool {:?} no longer exists", resolved.eth_tool_id)
                    })?;
                    let binding = crate::config_client::read_desired_state_document_in_txn(
                        txn,
                        crate::Collection::ChainKeyBinding,
                        &resolved.principal_did,
                        binding_id,
                    )
                    .await?
                    .ok_or_else(|| anyhow!("chain key binding {binding_id:?} does not exist"))?;
                    Ok::<_, anyhow::Error>((
                        serde_json::from_value(tool)?,
                        serde_json::from_value(binding)?,
                    ))
                })
            },
        )
        .await?;
    if !tool.enabled {
        bail!("EthTool {:?} is disabled", resolved.eth_tool_id);
    }
    if tool.node_did != resolved.principal_did {
        bail!("EthTool {:?} belongs to another node", resolved.eth_tool_id);
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
    if binding.node_did != resolved.principal_did {
        bail!("chain key binding {binding_id:?} belongs to another node");
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
        &binding.node_did,
        &binding.address,
        KEY_BACKEND_KEYRING,
        created_at,
    );
    if !crate::identity::verify_did_signature(&binding.node_did, &payload, &attestation)? {
        bail!("chain key binding {binding_id:?} has an invalid attestation");
    }
    let secret = Zeroizing::new(store.load(&binding_storage_key(&binding.node_did, binding_id))?);
    let address = address_from_secret(&secret)?;
    if !address.eq_ignore_ascii_case(&binding.address) {
        bail!("chain key material does not match binding {binding_id:?}");
    }
    Ok(*secret)
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
        return Err(EthError::UndeclaredModelParam {
            extra: extra.to_string(),
        });
    }

    let mut out = Vec::with_capacity(params.len());
    for param in params {
        let value = match param.decl.source.trim() {
            "model" => {
                model
                    .get(&param.name)
                    .cloned()
                    .ok_or_else(|| EthError::MissingModelParam {
                        name: param.name.clone(),
                    })?
            }
            "fixed" => param.decl.value.clone().unwrap_or(Value::Null),
            "runtime" => {
                json!(self_address.ok_or(EthError::SelfAddressUnavailable)?)
            }
            other => {
                return Err(EthError::UnsupportedParamSource {
                    kind: other.to_string(),
                })
            }
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
            .ok_or_else(|| EthError::ParamAddressNotString {
                name: param.name.clone(),
            })?;
        if !allowlist
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(address))
        {
            return Err(EthError::ParamAddressNotAllowed {
                name: param.name.clone(),
                address: address.to_string(),
            });
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
            return Err(EthError::ParamNotInEnum {
                name: param.name.clone(),
                value: actual,
            });
        }
    }
    if param.solidity_type.starts_with("uint") {
        let actual = parse_uint(value)?;
        if let Some(min) = &param.decl.min {
            if actual < parse_uint(&json!(min))? {
                return Err(EthError::ParamBelowMin {
                    name: param.name.clone(),
                    min: min.clone(),
                });
            }
        }
        if let Some(max) = &param.decl.max {
            if actual > parse_uint(&json!(max))? {
                return Err(EthError::ParamExceedsMax {
                    name: param.name.clone(),
                    max: max.clone(),
                });
            }
        }
    } else if param.solidity_type.starts_with("int") {
        let actual = parse_int(value)?;
        if let Some(min) = &param.decl.min {
            if actual < parse_int(&json!(min))? {
                return Err(EthError::ParamBelowMin {
                    name: param.name.clone(),
                    min: min.clone(),
                });
            }
        }
        if let Some(max) = &param.decl.max {
            if actual > parse_int(&json!(max))? {
                return Err(EthError::ParamExceedsMax {
                    name: param.name.clone(),
                    max: max.clone(),
                });
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
        other => Err(EthError::EnumConstraintValue {
            value: other.clone(),
        }),
    }
}

fn gas_caps(max_gas: Option<u64>, max_fee_per_gas: Option<&str>) -> Result<GasCaps> {
    let max_fee_per_gas = match max_fee_per_gas
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(text) => Some(parse_u128(text).map_err(|error| EthError::MaxFeeParse {
            text: text.to_string(),
            error,
        })?),
        None => None,
    };
    Ok(GasCaps {
        max_gas,
        max_fee_per_gas,
    })
}

fn encode_function(function: &Function, args: &[Value]) -> Result<String> {
    if function.inputs.len() != args.len() {
        return Err(EthError::SignatureArgCount {
            signature: function.signature(),
            expected: function.inputs.len(),
            got: args.len(),
        });
    }
    let mut values = Vec::with_capacity(args.len());
    for (param, value) in function.inputs.iter().zip(args) {
        values.push(
            json_to_dyn(&param.ty, value).map_err(|source| EthError::EncodeArgument {
                name: param.name.clone(),
                ty: param.ty.clone(),
                source: Box::new(source),
            })?,
        );
    }
    let encoded = function
        .abi_encode_input(&values)
        .map_err(|error| EthError::AbiEncode {
            signature: function.signature(),
            error,
        })?;
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
        let text = value.as_str().ok_or(EthError::AddressNotString)?;
        let address: Address = text
            .parse::<Address>()
            .map_err(|error| EthError::AddressParse {
                text: text.to_string(),
                error,
            })?;
        return Ok(DynSolValue::Address(address));
    }
    if ty == "bool" {
        let flag = value
            .as_bool()
            .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
            .ok_or(EthError::BoolExpected)?;
        return Ok(DynSolValue::Bool(flag));
    }
    if ty == "string" {
        let text = value.as_str().ok_or(EthError::StringExpected)?;
        return Ok(DynSolValue::String(text.to_string()));
    }
    if let Some(size) = ty.strip_prefix("bytes").filter(|size| !size.is_empty()) {
        let size: usize = size.parse().map_err(|source| EthError::FixedBytesType {
            ty: ty.to_string(),
            source,
        })?;
        if !(1..=32).contains(&size) {
            return Err(EthError::FixedBytesSize { size });
        }
        let text = value
            .as_str()
            .ok_or_else(|| EthError::HexStringExpected { ty: ty.to_string() })?;
        let bytes = decode_hex(text)?;
        if bytes.len() != size {
            return Err(EthError::FixedBytesLength {
                ty: ty.to_string(),
                size,
                got: bytes.len(),
            });
        }
        let mut padded = [0u8; 32];
        padded[..size].copy_from_slice(&bytes);
        return Ok(DynSolValue::FixedBytes(padded.into(), size));
    }
    if ty == "bytes" {
        let text = value.as_str().ok_or(EthError::BytesHexExpected)?;
        return Ok(DynSolValue::Bytes(decode_hex(text)?));
    }
    if let Some(bits) = integer_bit_width(ty, false) {
        let n = parse_uint(value)?;
        if bits < 256 && n >= (U256::from(1u8) << bits) {
            return Err(EthError::IntegerOutOfRange {
                ty: ty.to_string(),
                value: n.to_string(),
                bits,
            });
        }
        return Ok(DynSolValue::Uint(n, bits));
    }
    if let Some(bits) = integer_bit_width(ty, true) {
        let n = parse_int(value)?;
        if bits < 256 {
            let limit = I256::try_from(1u8).expect("one") << (bits - 1);
            if n >= limit || n < -limit {
                return Err(EthError::IntegerOutOfRange {
                    ty: ty.to_string(),
                    value: n.to_string(),
                    bits,
                });
            }
        }
        return Ok(DynSolValue::Int(n, bits));
    }
    Err(EthError::UnsupportedSolidityType { ty: ty.to_string() })
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

fn integer_bit_width(ty: &str, signed: bool) -> Option<usize> {
    let prefix = if signed { "int" } else { "uint" };
    let suffix = ty.strip_prefix(prefix)?;
    if signed && ty.starts_with("uint") {
        return None;
    }
    if suffix.is_empty() {
        return Some(256);
    }
    let bits = suffix.parse::<usize>().ok()?;
    ((8..=256).contains(&bits) && bits % 8 == 0).then_some(bits)
}

fn parse_uint(value: &Value) -> Result<U256> {
    match value {
        Value::String(text) => text
            .parse::<U256>()
            .or_else(|_| U256::from_str_radix(text.trim_start_matches("0x"), 16))
            .map_err(|error| EthError::UintParse {
                text: text.to_string(),
                error,
            }),
        Value::Number(n) => {
            if n.as_f64().is_some_and(|f| f.fract() != 0.0) {
                return Err(EthError::UintFloat { n: n.clone() });
            }
            n.as_u64()
                .map(U256::from)
                .ok_or_else(|| EthError::UintNotInteger { n: n.clone() })
        }
        other => Err(EthError::UintExpected { got: other.clone() }),
    }
}

fn parse_int(value: &Value) -> Result<I256> {
    match value {
        Value::String(text) => text.parse::<I256>().map_err(|error| EthError::IntParse {
            text: text.to_string(),
            error,
        }),
        Value::Number(n) => n
            .as_i64()
            .map(I256::try_from)
            .and_then(Result::ok)
            .ok_or_else(|| EthError::IntNotInteger { n: n.clone() }),
        other => Err(EthError::IntExpected { got: other.clone() }),
    }
}

fn decode_hex(value: &str) -> Result<Vec<u8>> {
    let hex = value.strip_prefix("0x").unwrap_or(value).trim();
    if !hex.len().is_multiple_of(2) {
        return Err(EthError::OddHex {
            value: value.to_string(),
        });
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(EthError::HexByte))
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

    use super::*;

    async fn signing_fixture() -> (
        Arc<EmbeddedNode>,
        ChainKeyBindingDocument,
        ResolvedEthCall,
        super::super::keys::MemoryChainKeyStore,
    ) {
        use crate::identity::NodeIdentity;
        let dir = tempfile::tempdir().unwrap();
        let identity =
            crate::identity::KeyIdentity::load_or_create(dir.path().join("key"), None).unwrap();
        let node = Arc::new(
            EmbeddedNode::builder()
                .with_node_identity_did(identity.did())
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let secret = [1u8; 32];
        let address = address_from_secret(&secret).unwrap();
        let created = "2026-09-09T00:00:00Z";
        let binding = ChainKeyBindingDocument {
            binding_id: "signing".into(),
            node_did: identity.did().into(),
            address: address.clone(),
            key_backend: Some(KEY_BACKEND_KEYRING.into()),
            attestation: Some(super::super::keys::encode_attestation(
                &identity
                    .sign(&attestation_payload(
                        "signing",
                        identity.did(),
                        &address,
                        KEY_BACKEND_KEYRING,
                        created,
                    ))
                    .await
                    .unwrap(),
            )),
            created_at: Some(created.into()),
            revoked_at: None,
            tags: vec!["payments".into()],
        };
        for owner in ["did:test:foreign", identity.did()] {
            let mut record = binding.clone();
            record.node_did = owner.into();
            let response = node
                .execute(&crate::create_chain_key_binding_mutation(&record).unwrap())
                .await;
            assert!(!response.has_errors(), "{:?}", response.errors);
            let value = json!({"tool_id":"payments","node_did":owner,"chain_id":8453,"key_binding_id":"signing","enabled":true});
            let input = gents_protocol::graphql::graphql_input_literal(&value).unwrap();
            let response = node
                .execute(&format!(
                    "mutation {{ create_EthTool(input: {input}) {{ _docID }} }}"
                ))
                .await;
            assert!(!response.has_errors(), "{:?}", response.errors);
        }
        let store = super::super::keys::MemoryChainKeyStore::default();
        store
            .store_new(&binding_storage_key(identity.did(), "signing"), &secret)
            .unwrap();
        let resolved = ResolvedEthCall {
            eth_tool_id: "payments".into(),
            tool_name: "send".into(),
            chain_id: 8453,
            rpc_url: "http://unused".into(),
            rpc_timeout: std::time::Duration::from_secs(30),
            description: String::new(),
            kind: ResolvedCallKind::Write {
                to: None,
                function: None,
                params: Vec::new(),
            },
            principal_did: identity.did().into(),
            binding_id: Some("signing".into()),
            caps: GasCaps::default(),
        };
        (node, binding, resolved, store)
    }

    #[tokio::test]
    async fn signing_reads_exact_scope_and_checks_attestation_revocation_and_material() {
        let (node, mut binding, resolved, store) = signing_fixture().await;
        assert_eq!(
            load_signing_key_with_store(&node, &resolved, &store)
                .await
                .unwrap(),
            [1u8; 32]
        );
        let valid = binding.clone();
        binding.attestation = Some("00".into());
        crate::upsert_chain_key_binding(&node, &binding)
            .await
            .unwrap();
        assert!(load_signing_key_with_store(&node, &resolved, &store)
            .await
            .is_err());
        crate::upsert_chain_key_binding(&node, &valid)
            .await
            .unwrap();
        store
            .delete(&binding_storage_key(&valid.node_did, "signing"))
            .unwrap();
        store
            .store_new(&binding_storage_key(&valid.node_did, "signing"), &[2u8; 32])
            .unwrap();
        assert!(load_signing_key_with_store(&node, &resolved, &store)
            .await
            .unwrap_err()
            .to_string()
            .contains("does not match"));
        store
            .delete(&binding_storage_key(&valid.node_did, "signing"))
            .unwrap();
        store
            .store_new(&binding_storage_key(&valid.node_did, "signing"), &[1u8; 32])
            .unwrap();
        binding = valid.clone();
        binding.revoked_at = Some("revoked".into());
        crate::upsert_chain_key_binding(&node, &binding)
            .await
            .unwrap();
        crate::upsert_chain_key_binding(&node, &valid)
            .await
            .unwrap();
        assert!(load_signing_key_with_store(&node, &resolved, &store)
            .await
            .unwrap_err()
            .to_string()
            .contains("revoked"));
        let records =
            crate::document_config::list_chain_key_binding_records(&node, &binding.node_did)
                .await
                .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].1.tags, vec!["payments"]);
        assert_eq!(records[0].1.revoked_at.as_deref(), Some("revoked"));
    }

    #[tokio::test]
    async fn signing_identity_is_unique_per_owner() {
        for collection in ["ChainKeyBinding", "EthTool"] {
            let (node, binding, resolved, store) = signing_fixture().await;
            let mut value = if collection == "ChainKeyBinding" {
                serde_json::to_value(&binding).unwrap()
            } else {
                json!({"tool_id":"payments","node_did":binding.node_did,"chain_id":8453,"key_binding_id":"signing","enabled":true})
            };
            value["tags"] = json!(["duplicate"]);
            let input = gents_protocol::graphql::graphql_input_literal(&value).unwrap();
            let response = node
                .execute(&format!(
                    "mutation {{ create_{collection}(input: {input}) {{_docID}} }}"
                ))
                .await;
            assert!(
                response.has_errors(),
                "{collection}: duplicate identity was accepted"
            );
            load_signing_key_with_store(&node, &resolved, &store)
                .await
                .expect("the original unique signing identity remains usable");
        }
    }
}
