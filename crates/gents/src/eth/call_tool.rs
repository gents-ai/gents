//! Generated read and `any_read` tools. ABI declarations are compiled once
//! when the runtime snapshot is built, then reused for schema and execution.

use std::collections::{BTreeMap, BTreeSet};

use alloy_dyn_abi::{DynSolValue, FunctionExt, JsonAbiExt};
use alloy_json_abi::Function;
use alloy_primitives::{Address, I256, U256};
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::llm::tool::{BoxFuture, ToolDefinition, ToolDyn, ToolError};
use crate::tool_call_lifecycle::FailureClass;

use super::calls::{compile_params, parse_abi_function, require_address, CallDecl, CompiledParam};
use super::rpc::HttpEthRpc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEthCall {
    pub(crate) tool_name: String,
    pub(crate) chain_id: u64,
    pub(crate) rpc_url: String,
    pub(crate) description: String,
    pub(crate) kind: ResolvedCallKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResolvedCallKind {
    AnyRead,
    Declared {
        to: String,
        function: Function,
        params: Vec<CompiledParam>,
    },
}

impl ResolvedEthCall {
    pub(crate) fn from_decls(
        tool_id: &str,
        chain_id: u64,
        rpc_url: &str,
        decls: &[CallDecl],
    ) -> Result<Vec<Self>> {
        let mut out = Vec::new();
        for decl in decls {
            match decl {
                CallDecl::AnyRead => out.push(Self {
                    tool_name: format!("{tool_id}_any_read"),
                    chain_id,
                    rpc_url: rpc_url.to_string(),
                    description: "ABI-encoded primitive eth_call. Supply a signature and arguments; raw calldata is never accepted."
                        .to_string(),
                    kind: ResolvedCallKind::AnyRead,
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
}

impl EthCallTool {
    pub(crate) fn new(resolved: ResolvedEthCall) -> Self {
        Self { resolved }
    }
}

impl ToolDyn for EthCallTool {
    fn name(&self) -> String {
        self.resolved.tool_name.clone()
    }

    fn definition(&self, _prompt: String) -> BoxFuture<'_, ToolDefinition> {
        let resolved = self.resolved.clone();
        Box::pin(async move {
            let parameters = match &resolved.kind {
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
                ResolvedCallKind::Declared { params, .. } => declared_schema(params),
            };
            ToolDefinition {
                name: resolved.tool_name,
                description: resolved.description,
                parameters,
            }
        })
    }

    fn call(&self, args: String) -> BoxFuture<'_, Result<String, ToolError>> {
        let resolved = self.resolved.clone();
        Box::pin(async move { execute_call(&resolved, &args).await })
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

async fn execute_call(resolved: &ResolvedEthCall, args: &str) -> Result<String, ToolError> {
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
            let arg_values = bind_params(params, &parsed)
                .map_err(|error| reported(FailureClass::PolicyDenied, error.to_string()))?;
            let data = encode_function(function, &arg_values)
                .map_err(|error| reported(FailureClass::ArgumentInvalid, error.to_string()))?;
            let result = client
                .eth_call(to, &data, None)
                .await
                .map_err(|error| reported(FailureClass::Transport, error.to_string()))?;
            decode_or_hex(function, &result)
        }
    }
}

fn reported(class: FailureClass, text: String) -> ToolError {
    ToolError::ReportedFailure { class, text }
}

fn bind_params(params: &[CompiledParam], model: &BTreeMap<String, Value>) -> Result<Vec<Value>> {
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
        let tool = EthCallTool::new(ResolvedEthCall {
            tool_name: "base_any_read".to_string(),
            chain_id: 8453,
            rpc_url: "http://127.0.0.1".to_string(),
            description: "any".to_string(),
            kind: ResolvedCallKind::AnyRead,
        });
        let definition = futures::executor::block_on(tool.definition(String::new()));
        assert!(definition.parameters["properties"].get("data").is_none());
        assert!(definition.parameters["properties"]
            .get("calldata")
            .is_none());
        assert_eq!(definition.parameters["additionalProperties"], false);
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
        let bound = bind_params(&params, &model).expect("bind");
        assert_eq!(bound[0], model["recipient"]);
        assert_eq!(bound[1], json!(5));

        let mut above_max = model.clone();
        above_max.insert("qty".to_string(), json!(11));
        assert!(bind_params(&params, &above_max).is_err());

        let mut outside_enum = model;
        outside_enum.insert("qty".to_string(), json!(7));
        assert!(bind_params(&params, &outside_enum).is_err());
    }
}
