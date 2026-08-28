//! Parse and compile `EthTool.calls` declarations.

use std::collections::{BTreeMap, BTreeSet};

use alloy_json_abi::Function;
use alloy_primitives::{I256, U256};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::document_config::deserialize_dual_shape;

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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
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
        .map_err(|error| anyhow!("EthTool.calls: {error}"))
}

pub fn validate_call_decls(decls: &[CallDecl]) -> Result<()> {
    let mut names = BTreeSet::new();
    let mut any_read = false;
    for decl in decls {
        match decl {
            CallDecl::AnyRead => {
                if any_read {
                    bail!("EthTool.calls contains any_read more than once");
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
        }
    }
    Ok(())
}

pub(crate) fn compile_params(
    function: &Function,
    params: &BTreeMap<String, ParamDecl>,
    require_write_limits: bool,
) -> Result<Vec<CompiledParam>> {
    if function.inputs.len() != params.len() {
        bail!(
            "ABI {} has {} inputs but {} parameter declarations",
            function.signature(),
            function.inputs.len(),
            params.len()
        );
    }

    let mut seen = BTreeSet::new();
    let mut compiled = Vec::with_capacity(function.inputs.len());
    for input in &function.inputs {
        let name = input.name.trim();
        if name.is_empty() {
            bail!(
                "declared call ABI input {} is unnamed; use a named signature such as address recipient",
                input.ty
            );
        }
        if !seen.insert(name.to_string()) {
            bail!("declared call ABI has duplicate input name {name:?}");
        }
        let decl = params
            .get(name)
            .ok_or_else(|| anyhow!("ABI input {name:?} has no matching parameter declaration"))?;
        validate_param(name, &input.ty, decl, require_write_limits)?;
        compiled.push(CompiledParam {
            name: name.to_string(),
            solidity_type: input.ty.clone(),
            decl: decl.clone(),
        });
    }
    if let Some(extra) = params.keys().find(|name| !seen.contains(name.as_str())) {
        bail!("parameter declaration {extra:?} is not an ABI input");
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
    if !matches!(source, "model" | "fixed") {
        bail!("call param {name} has unsupported source {source:?}");
    }
    if source == "fixed" && param.value.is_none() {
        bail!("call param {name} source=fixed requires value");
    }
    if !supported_primitive_type(solidity_type) {
        bail!("call param {name} uses unsupported Solidity type {solidity_type}");
    }

    let is_address = solidity_type == "address";
    let is_integer = integer_kind(solidity_type).is_some();
    if let Some(allowlist) = &param.address_allowlist {
        if !is_address {
            bail!("call param {name} has address_allowlist but type is {solidity_type}");
        }
        if allowlist.is_empty() {
            bail!("call param {name} address_allowlist is empty");
        }
        for address in allowlist {
            require_address(address, &format!("param {name} address_allowlist"))?;
        }
    }
    if (param.min.is_some() || param.max.is_some()) && !is_integer {
        bail!("call param {name} has min/max but type is {solidity_type}");
    }
    validate_numeric_bounds(
        name,
        solidity_type,
        param.min.as_deref(),
        param.max.as_deref(),
    )?;
    if param.enum_values.as_ref().is_some_and(Vec::is_empty) {
        bail!("call param {name} enum is empty");
    }

    if require_write_limits && source == "model" {
        if is_address
            && param
                .address_allowlist
                .as_ref()
                .is_none_or(|values| values.is_empty())
        {
            bail!("write param {name} address has no allowlist (deny-by-default)");
        }
        if is_integer && param.max.is_none() {
            bail!("write param {name} integer has no max (deny-by-default)");
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
                bail!("call param {name} min exceeds max");
            }
        }
        Some(IntegerKind::Signed) => {
            let min = min.map(parse_i256).transpose()?;
            let max = max.map(parse_i256).transpose()?;
            if min.zip(max).is_some_and(|(min, max)| min > max) {
                bail!("call param {name} min exceeds max");
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
    } else if let Some(suffix) = solidity_type.strip_prefix("int") {
        (IntegerKind::Signed, suffix)
    } else {
        return None;
    };
    if suffix.is_empty() {
        return Some(kind);
    }
    let bits = suffix.parse::<u16>().ok()?;
    ((8..=256).contains(&bits) && bits % 8 == 0).then_some(kind)
}

fn supported_primitive_type(solidity_type: &str) -> bool {
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
        .with_context(|| format!("invalid unsigned integer bound {value:?}"))
}

fn parse_i256(value: &str) -> Result<I256> {
    value
        .parse::<I256>()
        .with_context(|| format!("invalid signed integer bound {value:?}"))
}

fn require_tool_name(tool_name: &str, names: &mut BTreeSet<String>) -> Result<()> {
    let name = tool_name.trim();
    if name.is_empty() {
        bail!("call tool_name is empty");
    }
    if !names.insert(name.to_string()) {
        bail!("duplicate call tool_name {name}");
    }
    Ok(())
}

pub(crate) fn require_address(value: &str, field: &str) -> Result<()> {
    let hex = value.strip_prefix("0x").unwrap_or(value).trim();
    if hex.len() != 40 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("{field} must be a 20-byte 0x-address, got {value:?}");
    }
    Ok(())
}

pub(crate) fn parse_abi_function(signature: &str) -> Result<Function> {
    let trimmed = signature.trim();
    if trimmed.is_empty() {
        bail!("call signature is empty");
    }
    let with_fn = if trimmed.starts_with("function ") {
        trimmed.to_string()
    } else {
        format!("function {trimmed}")
    };
    Function::parse(&with_fn)
        .map_err(|error| anyhow!("invalid ABI signature {signature:?}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_calls_are_deny() {
        assert!(parse_call_decls(None).unwrap().is_empty());
        assert!(parse_call_decls(Some(&[])).unwrap().is_empty());
    }

    #[test]
    fn declared_read_requires_named_abi_inputs() {
        let read = r#"{"kind":"read","tool_name":"balance","to":"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913","signature":"balanceOf(address)","params":{"account":{"source":"model"}}}"#;
        let decls = parse_call_decls(Some(&[read.to_string()])).unwrap();
        assert!(validate_call_decls(&decls)
            .unwrap_err()
            .to_string()
            .contains("unnamed"));
    }

    #[test]
    fn compiled_params_follow_abi_order_not_map_order() {
        let params = BTreeMap::from([
            ("amount".to_string(), ParamDecl::default()),
            ("recipient".to_string(), ParamDecl::default()),
        ]);
        let function = parse_abi_function("transfer(address recipient,uint256 amount)").unwrap();
        let compiled = compile_params(&function, &params, false).unwrap();
        assert_eq!(
            compiled
                .iter()
                .map(|param| param.name.as_str())
                .collect::<Vec<_>>(),
            vec!["recipient", "amount"]
        );
    }

    #[test]
    fn constraint_types_are_abi_derived() {
        let params = BTreeMap::from([(
            "beneficiary".to_string(),
            ParamDecl {
                address_allowlist: Some(vec![
                    "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".to_string()
                ]),
                ..ParamDecl::default()
            },
        )]);
        let function = parse_abi_function("pay(address beneficiary)").unwrap();
        compile_params(&function, &params, true).unwrap();

        let unbounded = BTreeMap::from([("qty".to_string(), ParamDecl::default())]);
        let function = parse_abi_function("mint(uint256 qty)").unwrap();
        assert!(compile_params(&function, &unbounded, true)
            .unwrap_err()
            .to_string()
            .contains("no max"));
    }

    #[test]
    fn unknown_future_call_kinds_are_rejected() {
        let error = parse_call_decls(Some(
            &[r#"{"kind":"write","tool_name":"send"}"#.to_string()],
        ))
        .unwrap_err();
        assert!(error.to_string().contains("EthTool.calls"));
    }
}
