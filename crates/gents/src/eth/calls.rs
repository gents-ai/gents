use std::collections::{BTreeMap, BTreeSet};

use alloy_json_abi::Function;
use alloy_primitives::{I256, U256};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::document_config::deserialize_dual_shape;

use super::error::{EthError, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CallDecl {
    AnyRead,
    Read {
        tool_name: String,
        to: String,
        signature: String,
        #[serde(default)]
        params: BTreeMap<String, ParamDecl>,
        #[serde(default)]
        description: Option<String>,
    },
    Write {
        tool_name: String,
        to: String,
        signature: String,
        #[serde(default)]
        params: BTreeMap<String, ParamDecl>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        max_gas: Option<u64>,
        #[serde(default)]
        max_fee_per_gas: Option<String>,
    },
    NativeTransfer {
        tool_name: String,
        #[serde(default)]
        params: BTreeMap<String, ParamDecl>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        max_gas: Option<u64>,
        #[serde(default)]
        max_fee_per_gas: Option<String>,
    },
    SignTypedData {
        tool_name: String,
        domain: Value,
        types: Value,
        primary_type: String,
        #[serde(default)]
        params: BTreeMap<String, ParamDecl>,
        #[serde(default)]
        description: Option<String>,
    },
}

impl CallDecl {
    pub(crate) fn requires_key_binding(&self) -> bool {
        matches!(
            self,
            Self::Write { .. } | Self::NativeTransfer { .. } | Self::SignTypedData { .. }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParamDecl {
    #[serde(default = "default_model_source")]
    pub source: String,
    #[serde(default)]
    pub value: Option<Value>,
    #[serde(default)]
    pub address_allowlist: Option<Vec<String>>,
    #[serde(default)]
    pub min: Option<String>,
    #[serde(default)]
    pub max: Option<String>,
    #[serde(rename = "enum", default)]
    pub enum_values: Option<Vec<String>>,
    #[serde(default)]
    pub runtime: Option<String>,
}

impl Default for ParamDecl {
    fn default() -> Self {
        Self {
            source: default_model_source(),
            value: None,
            address_allowlist: None,
            min: None,
            max: None,
            enum_values: None,
            runtime: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompiledParam {
    pub(crate) name: String,
    pub(crate) solidity_type: String,
    pub(crate) decl: ParamDecl,
}

fn default_model_source() -> String {
    "model".to_string()
}

pub fn parse_call_decls(raw: Option<&[String]>) -> Result<Vec<CallDecl>> {
    if raw.is_none() || raw.is_some_and(|items| items.is_empty()) {
        return Ok(Vec::new());
    }
    let values: Vec<Value> = raw
        .unwrap_or(&[])
        .iter()
        .map(|item| serde_json::from_str(item).unwrap_or(Value::String(item.clone())))
        .collect();
    deserialize_dual_shape(Some(Value::Array(values)), "EthTool.calls")
        .map_err(|error| EthError::CallsDecode { error })
}

pub fn validate_call_decls(decls: &[CallDecl], chain_id: Option<u64>) -> Result<()> {
    let mut names = BTreeSet::new();
    let mut any_read = false;
    for decl in decls {
        match decl {
            CallDecl::AnyRead => {
                if any_read {
                    return Err(EthError::AnyReadRepeated);
                }
                any_read = true;
            }
            CallDecl::Read {
                tool_name,
                to,
                signature,
                params,
                ..
            } => {
                require_tool_name(tool_name, &mut names)?;
                require_address(to, "read.to")?;
                let function = parse_abi_function(signature)?;
                compile_params(&function, params, false)?;
            }
            CallDecl::Write {
                tool_name,
                to,
                signature,
                params,
                max_gas,
                max_fee_per_gas,
                ..
            } => {
                require_tool_name(tool_name, &mut names)?;
                require_address(to, "write.to")?;
                validate_gas_caps(tool_name, *max_gas, max_fee_per_gas.as_deref())?;
                let function = parse_abi_function(signature)?;
                compile_params(&function, params, true)?;
            }
            CallDecl::NativeTransfer {
                tool_name,
                params,
                max_gas,
                max_fee_per_gas,
                ..
            } => {
                require_tool_name(tool_name, &mut names)?;
                validate_gas_caps(tool_name, *max_gas, max_fee_per_gas.as_deref())?;
                let function = native_transfer_function()?;
                compile_params(&function, params, true)?;
            }
            CallDecl::SignTypedData {
                tool_name,
                domain,
                types,
                primary_type,
                params,
                ..
            } => {
                require_tool_name(tool_name, &mut names)?;
                if primary_type.trim().is_empty() {
                    return Err(EthError::SignTypedPrimaryEmpty);
                }
                if !domain.is_object() {
                    return Err(EthError::SignTypedDomainNotObject);
                }
                if !types.is_object() {
                    return Err(EthError::SignTypedTypesNotObject);
                }
                compile_typed_params(types, primary_type, params, true)?;
                match typed_domain_chain_id(domain)? {
                    Some(domain_chain_id)
                        if chain_id.is_none_or(|expected| expected == domain_chain_id) => {}
                    Some(domain_chain_id) => {
                        return Err(EthError::SignTypedDomainMismatch {
                            tool_name: tool_name.clone(),
                            domain_chain_id,
                            chain_id: chain_id.unwrap(),
                        })
                    }
                    None => {
                        return Err(EthError::SignTypedDomainMissing {
                            tool_name: tool_name.clone(),
                        })
                    }
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn typed_domain_chain_id(domain: &Value) -> Result<Option<u64>> {
    let Some(value) = domain.get("chainId") else {
        return Ok(None);
    };
    match value {
        Value::Number(number) => number
            .as_u64()
            .map(Some)
            .ok_or(EthError::Eip712ChainIdNotPositive),
        Value::String(text) => {
            let parsed = if let Some(hex) = text.strip_prefix("0x") {
                u64::from_str_radix(hex, 16)
            } else {
                text.parse()
            };
            parsed
                .map(Some)
                .map_err(|error| EthError::Eip712ChainIdParse {
                    text: text.clone(),
                    error,
                })
        }
        other => Err(EthError::Eip712ChainIdType { got: other.clone() }),
    }
}

fn validate_gas_caps(
    tool_name: &str,
    max_gas: Option<u64>,
    max_fee_per_gas: Option<&str>,
) -> Result<()> {
    if max_gas.is_none_or(|value| value == 0) {
        return Err(EthError::WriteRequiresMaxGas {
            tool_name: tool_name.to_string(),
        });
    }
    let fee = max_fee_per_gas
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| EthError::WriteRequiresMaxFee {
            tool_name: tool_name.to_string(),
        })?;
    if parse_u128(fee).map_err(|source| EthError::WriteMaxFeeParse {
        tool_name: tool_name.to_string(),
        source,
    })? == 0
    {
        return Err(EthError::WriteMaxFeeZero {
            tool_name: tool_name.to_string(),
        });
    }
    Ok(())
}

pub(crate) fn parse_u128(value: &str) -> std::result::Result<u128, std::num::ParseIntError> {
    value
        .parse::<u128>()
        .or_else(|_| u128::from_str_radix(value.trim_start_matches("0x"), 16))
}

pub(crate) fn compile_params(
    function: &Function,
    params: &BTreeMap<String, ParamDecl>,
    require_write_limits: bool,
) -> Result<Vec<CompiledParam>> {
    if function.inputs.len() != params.len() {
        return Err(EthError::ParamCountMismatch {
            abi: function.signature(),
            inputs: function.inputs.len(),
            params: params.len(),
        });
    }

    let mut seen = BTreeSet::new();
    let mut compiled = Vec::with_capacity(function.inputs.len());
    for input in &function.inputs {
        let name = input.name.trim();
        if name.is_empty() {
            return Err(EthError::UnnamedAbiInput {
                ty: input.ty.clone(),
            });
        }
        if !seen.insert(name.to_string()) {
            return Err(EthError::DuplicateAbiInput {
                name: name.to_string(),
            });
        }
        let decl = params
            .get(name)
            .ok_or_else(|| EthError::AbiInputUndeclared {
                name: name.to_string(),
            })?;
        validate_param(name, &input.ty, decl, require_write_limits)?;
        compiled.push(CompiledParam {
            name: name.to_string(),
            solidity_type: input.ty.clone(),
            decl: decl.clone(),
        });
    }
    if let Some(extra) = params.keys().find(|name| !seen.contains(name.as_str())) {
        return Err(EthError::ParamUndeclaredInAbi {
            extra: extra.to_string(),
        });
    }
    Ok(compiled)
}

fn validate_param(
    name: &str,
    solidity_type: &str,
    param: &ParamDecl,
    require_write_limits: bool,
) -> Result<()> {
    let source = param.source.trim();
    if !matches!(source, "model" | "fixed" | "runtime") {
        return Err(EthError::ParamSourceUnsupported {
            name: name.to_string(),
            kind: source.to_string(),
        });
    }
    if source == "fixed" && param.value.is_none() {
        return Err(EthError::ParamFixedNeedsValue {
            name: name.to_string(),
        });
    }
    if source == "runtime" && param.runtime.as_deref() != Some("self_address") {
        return Err(EthError::ParamRuntimeSource {
            name: name.to_string(),
        });
    }
    if !supported_primitive_type(solidity_type) {
        return Err(EthError::ParamUnsupportedType {
            name: name.to_string(),
            solidity_type: solidity_type.to_string(),
        });
    }

    let is_address = solidity_type == "address";
    let is_integer = integer_kind(solidity_type).is_some();
    let is_unbounded_data = solidity_type == "string" || solidity_type.starts_with("bytes");
    if source == "runtime" && !is_address {
        return Err(EthError::ParamRuntimeNeedsAddress {
            name: name.to_string(),
        });
    }
    if let Some(allowlist) = &param.address_allowlist {
        if !is_address {
            return Err(EthError::ParamAllowlistNotAddress {
                name: name.to_string(),
                solidity_type: solidity_type.to_string(),
            });
        }
        if allowlist.is_empty() {
            return Err(EthError::ParamAllowlistEmpty {
                name: name.to_string(),
            });
        }
        for address in allowlist {
            require_address(address, &format!("param {name} address_allowlist"))?;
        }
    }
    if (param.min.is_some() || param.max.is_some()) && !is_integer {
        return Err(EthError::ParamMinMaxNotInteger {
            name: name.to_string(),
            solidity_type: solidity_type.to_string(),
        });
    }
    validate_numeric_bounds(
        name,
        solidity_type,
        param.min.as_deref(),
        param.max.as_deref(),
    )?;
    if param.enum_values.as_ref().is_some_and(Vec::is_empty) {
        return Err(EthError::ParamEnumEmpty {
            name: name.to_string(),
        });
    }

    if require_write_limits && source == "model" {
        if is_address
            && param
                .address_allowlist
                .as_ref()
                .is_none_or(|values| values.is_empty())
        {
            return Err(EthError::WriteAddressNoAllowlist {
                name: name.to_string(),
            });
        }
        if is_integer && param.max.is_none() {
            return Err(EthError::WriteIntegerNoMax {
                name: name.to_string(),
            });
        }
        if is_unbounded_data
            && param
                .enum_values
                .as_ref()
                .is_none_or(|values| values.is_empty())
        {
            return Err(EthError::WriteDataNoEnum {
                name: name.to_string(),
                solidity_type: solidity_type.to_string(),
            });
        }
    }
    Ok(())
}

fn validate_numeric_bounds(
    name: &str,
    solidity_type: &str,
    min: Option<&str>,
    max: Option<&str>,
) -> Result<()> {
    match integer_kind(solidity_type) {
        Some(IntegerKind::Unsigned) => {
            let min = min.map(parse_u256).transpose()?;
            let max = max.map(parse_u256).transpose()?;
            if min.zip(max).is_some_and(|(min, max)| min > max) {
                return Err(EthError::ParamMinExceedsMax {
                    name: name.to_string(),
                });
            }
        }
        Some(IntegerKind::Signed) => {
            let min = min.map(parse_i256).transpose()?;
            let max = max.map(parse_i256).transpose()?;
            if min.zip(max).is_some_and(|(min, max)| min > max) {
                return Err(EthError::ParamMinExceedsMax {
                    name: name.to_string(),
                });
            }
        }
        None => {}
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IntegerKind {
    Unsigned,
    Signed,
}

fn integer_kind(solidity_type: &str) -> Option<IntegerKind> {
    let (kind, suffix) = if let Some(suffix) = solidity_type.strip_prefix("uint") {
        (IntegerKind::Unsigned, suffix)
    } else {
        (IntegerKind::Signed, solidity_type.strip_prefix("int")?)
    };
    if suffix.is_empty() {
        return Some(kind);
    }
    let bits = suffix.parse::<u16>().ok()?;
    ((8..=256).contains(&bits) && bits % 8 == 0).then_some(kind)
}

pub(crate) fn supported_primitive_type(solidity_type: &str) -> bool {
    solidity_type == "address"
        || solidity_type == "bool"
        || solidity_type == "string"
        || solidity_type == "bytes"
        || solidity_type.strip_prefix("bytes").is_some_and(|size| {
            size.parse::<usize>()
                .is_ok_and(|size| (1..=32).contains(&size))
        })
        || integer_kind(solidity_type).is_some()
}

fn parse_u256(value: &str) -> Result<U256> {
    value
        .parse::<U256>()
        .or_else(|_| U256::from_str_radix(value.trim_start_matches("0x"), 16))
        .map_err(|source| EthError::UintBound {
            value: value.to_string(),
            source,
        })
}

fn parse_i256(value: &str) -> Result<I256> {
    value.parse::<I256>().map_err(|source| EthError::IntBound {
        value: value.to_string(),
        source,
    })
}

fn require_tool_name(tool_name: &str, names: &mut BTreeSet<String>) -> Result<()> {
    let name = tool_name.trim();
    if name.is_empty() {
        return Err(EthError::ToolNameEmpty);
    }
    if !names.insert(name.to_string()) {
        return Err(EthError::DuplicateToolName {
            name: name.to_string(),
        });
    }
    Ok(())
}

pub(crate) fn require_address(value: &str, field: &str) -> Result<()> {
    let hex = value.strip_prefix("0x").unwrap_or(value).trim();
    if hex.len() != 40 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(EthError::AddressFormat {
            field: field.to_string(),
            value: value.to_string(),
        });
    }
    Ok(())
}

pub(crate) fn parse_abi_function(signature: &str) -> Result<Function> {
    let trimmed = signature.trim();
    if trimmed.is_empty() {
        return Err(EthError::SignatureEmpty);
    }
    let with_fn = if trimmed.starts_with("function ") {
        trimmed.to_string()
    } else {
        format!("function {trimmed}")
    };
    Function::parse(&with_fn).map_err(|error| EthError::AbiSignatureInvalid {
        signature: signature.to_string(),
        error,
    })
}

pub(crate) fn native_transfer_function() -> Result<Function> {
    parse_abi_function("nativeTransfer(address to,uint256 value)")
}

pub(crate) fn compile_typed_params(
    types: &Value,
    primary_type: &str,
    params: &BTreeMap<String, ParamDecl>,
    require_write_limits: bool,
) -> Result<Vec<CompiledParam>> {
    let fields = types
        .get(primary_type)
        .and_then(Value::as_array)
        .ok_or_else(|| EthError::TypedTypesNoFieldList {
            primary_type: primary_type.to_string(),
        })?;
    if fields.len() != params.len() {
        return Err(EthError::TypedFieldCount {
            primary_type: primary_type.to_string(),
            fields: fields.len(),
            params: params.len(),
        });
    }
    let mut seen = BTreeSet::new();
    let mut compiled = Vec::with_capacity(fields.len());
    for field in fields {
        let name = field
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| EthError::TypedFieldNoName {
                primary_type: primary_type.to_string(),
            })?;
        let solidity_type = field
            .get("type")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| EthError::TypedFieldNoType {
                primary_type: primary_type.to_string(),
                name: name.to_string(),
            })?;
        if !seen.insert(name.to_string()) {
            return Err(EthError::TypedFieldDuplicate {
                primary_type: primary_type.to_string(),
                name: name.to_string(),
            });
        }
        let decl = params
            .get(name)
            .ok_or_else(|| EthError::TypedFieldUndeclared {
                name: name.to_string(),
            })?;
        validate_param(name, solidity_type, decl, require_write_limits)?;
        compiled.push(CompiledParam {
            name: name.to_string(),
            solidity_type: solidity_type.to_string(),
            decl: decl.clone(),
        });
    }
    Ok(compiled)
}
