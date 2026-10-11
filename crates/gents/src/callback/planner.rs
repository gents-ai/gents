//! Sealed Afterburner ActionPlan adapter. Callback ownership and journals stay
//! with the callback lifecycle; artifact execution belongs to PluginExecutor.

use std::collections::BTreeSet;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::workspace::{
    action_plan_canonical_json, canonical_json_string, parse_action_plan_json, ActionPlan,
    ACTION_PLAN_ABI,
};

use super::documents::CallbackModuleDoc;

const WASM_PAGE_BYTES: u64 = 65536;
const CALLBACK_MODULE_ID_DOMAIN: &[u8] = b"gents.callback.module.v1";

/// Host ceilings independent of CallbackModule fields. Over-ceiling → Denied.
pub const MAX_FUEL: u64 = 100_000_000;
pub const MAX_MEMORY_PAGES: u32 = 256;
pub const MAX_INPUT_BYTES: usize = 1_048_576;
pub const MAX_OUTPUT_BYTES: usize = 1_048_576;
pub const MAX_ARTIFACT_BYTES: usize = 16 * 1024 * 1024;

pub struct CallbackModuleLimits {
    pub fuel_limit: u64,
    pub memory_pages: u32,
    pub max_input_bytes: usize,
    pub max_output_bytes: usize,
}

/// Content-addressed id over decoded bytes + canonical JSON args + ABI.
/// Hashes the bytes themselves, never a host path.
pub fn compute_module_id(wasm: &[u8], args: &Value, abi_version: i64) -> Result<String, String> {
    let args_json = canonical_json_string(args)?;
    let mut hasher = Sha256::new();
    hasher.update(CALLBACK_MODULE_ID_DOMAIN);
    hasher.update(&(wasm.len() as u64).to_be_bytes());
    hasher.update(wasm);
    hasher.update(&(args_json.len() as u64).to_be_bytes());
    hasher.update(args_json.as_bytes());
    hasher.update(&(abi_version as u64).to_be_bytes());
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

pub fn decode_artifact_bytes(base64_text: &str) -> Result<Vec<u8>, String> {
    let trimmed = base64_text.trim();
    if trimmed.is_empty() {
        return Err("CallbackModule.artifact_bytes is empty".into());
    }
    STANDARD
        .decode(trimmed)
        .map_err(|error| format!("CallbackModule.artifact_bytes is not standard base64: {error}"))
}

pub fn parse_canonical_args(raw: Option<&str>) -> Result<Value, String> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(json!({}));
    };
    let value: Value = serde_json::from_str(raw)
        .map_err(|error| format!("CallbackModule.canonical_args is not JSON: {error}"))?;
    let _ = canonical_json_string(&value)?;
    Ok(value)
}

pub fn limits_from_module(module: &CallbackModuleDoc) -> Result<CallbackModuleLimits, String> {
    Ok(CallbackModuleLimits {
        fuel_limit: bounded_u64(module.fuel_limit, "fuel_limit", MAX_FUEL)?,
        memory_pages: bounded_u32(module.memory_pages, "memory_pages", MAX_MEMORY_PAGES)?,
        max_input_bytes: bounded_usize(module.max_input_bytes, "max_input_bytes", MAX_INPUT_BYTES)?,
        max_output_bytes: bounded_usize(
            module.max_output_bytes,
            "max_output_bytes",
            MAX_OUTPUT_BYTES,
        )?,
    })
}

/// Install/apply predicate for CallbackModule (fail closed).
///
/// v1 is an **installer allowlist**, not a cryptographic signature: `signer_did`
/// must be an enabled `Node` DID. `provenance` is a required non-empty
/// operator note (pack name, review URL, etc.), not a signature over bytes.
/// `CallbackBinding.principal_did` is the workspace writer and is not required
/// to equal `signer_did`. Call this before first use. Recovery that already
/// has a stored ActionPlan must not re-run it.
pub fn validate_callback_module(
    module: &CallbackModuleDoc,
    trusted_signers: &BTreeSet<String>,
) -> Result<(), String> {
    if module.module_id.trim().is_empty() {
        return Err("CallbackModule.module_id is missing".into());
    }
    let signer = module
        .signer_did
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "CallbackModule signer_did is missing".to_string())?;
    let provenance = module
        .provenance
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "CallbackModule provenance is missing".to_string())?;
    if trusted_signers.is_empty() {
        return Err("CallbackModule signer policy is missing: no trusted principals".into());
    }
    if !trusted_signers.contains(signer) {
        return Err(format!(
            "CallbackModule signer_did `{signer}` is not a trusted installer (allowlist, not a signature)"
        ));
    }
    tracing::debug!(
        signer_did = signer,
        provenance,
        module_id = %module.module_id,
        "CallbackModule installer allowlist accepted"
    );
    let abi = module.abi_version.unwrap_or(0);
    if abi != i64::from(ACTION_PLAN_ABI) {
        return Err(format!(
            "CallbackModule abi_version {abi} is unsupported (expected {})",
            ACTION_PLAN_ABI
        ));
    }
    let wasm = decode_artifact_bytes(module.artifact_bytes.as_deref().unwrap_or(""))?;
    require_artifact(&wasm)?;
    let args = parse_canonical_args(module.canonical_args.as_deref())?;
    let expected = compute_module_id(&wasm, &args, abi)?;
    if expected != module.module_id.trim() {
        return Err(format!(
            "CallbackModule.module_id does not match content-addressed id (expected {expected})"
        ));
    }
    let _ = limits_from_module(module)?;
    Ok(())
}

pub fn plan_from_module(
    module: &CallbackModuleDoc,
    source: &Value,
    capabilities: &BTreeSet<String>,
) -> Result<ActionPlan, String> {
    if !module.enabled {
        return Err("CallbackModule is disabled".into());
    }
    let wasm = decode_artifact_bytes(module.artifact_bytes.as_deref().unwrap_or(""))?;
    let args = parse_canonical_args(module.canonical_args.as_deref())?;
    let abi = module.abi_version.unwrap_or(0);
    if abi != i64::from(ACTION_PLAN_ABI)
        || compute_module_id(&wasm, &args, abi)? != module.module_id
    {
        return Err(
            "CallbackModule content address or ActionPlan ABI does not match artifact".into(),
        );
    }
    let limits = limits_from_module(module)?;
    let capabilities: Vec<String> = capabilities.iter().cloned().collect();
    let input = json!({
        "args": args,
        "capabilities": capabilities,
        "source": source,
    });
    let output = invoke_plugin_planner(&wasm, &limits, &input)?;
    let plan = parse_action_plan_json(&canonical_json_string(&output)?)?;
    let _ = action_plan_canonical_json(&plan)?;
    Ok(plan)
}

pub fn invoke_plugin_planner(
    artifact: &[u8],
    limits: &CallbackModuleLimits,
    input: &Value,
) -> Result<Value, String> {
    require_artifact(artifact)?;
    let input_len = canonical_json_string(input)?.len();
    if input_len > limits.max_input_bytes {
        return Err(format!(
            "planner input {input_len} bytes exceeds max_input_bytes {}",
            limits.max_input_bytes
        ));
    }
    let budget = crate::plugin::PluginBudget {
        fuel: Some(limits.fuel_limit),
        memory_bytes: u64::from(limits.memory_pages) * WASM_PAGE_BYTES,
        max_output_bytes: limits.max_output_bytes,
        ..Default::default()
    };
    let outcome =
        crate::plugin::executor::PluginExecutor::call_sealed_artifact(artifact, input, budget)
            .map_err(|error| format!("callback plugin denied: {error:#}"))?;
    if outcome.verdict != crate::plugin::PluginVerdict::Success {
        return Err(format!(
            "callback plugin {:?}: {}",
            outcome.verdict, outcome.diagnostics
        ));
    }
    Ok(outcome.output)
}

fn require_artifact(artifact: &[u8]) -> Result<(), String> {
    if artifact.len() > MAX_ARTIFACT_BYTES {
        return Err(format!(
            "callback artifact exceeds max_artifact_bytes {MAX_ARTIFACT_BYTES}"
        ));
    }
    crate::plugin::executor::PluginExecutor::validate_sealed_artifact(artifact).map_err(|error| {
        format!("callback requires a bounded Afterburner .afb artifact: {error:#}")
    })
}

fn bounded_u64(value: Option<i64>, field: &str, max: u64) -> Result<u64, String> {
    let n = match value {
        Some(n) if n > 0 => n as u64,
        _ => return Err(format!("CallbackModule.{field} must be a positive integer")),
    };
    if n > max {
        return Err(format!(
            "CallbackModule.{field} {n} exceeds host maximum {max}"
        ));
    }
    Ok(n)
}

fn bounded_u32(value: Option<i64>, field: &str, max: u32) -> Result<u32, String> {
    let n = bounded_u64(value, field, u64::from(max))?;
    u32::try_from(n).map_err(|_| format!("CallbackModule.{field} exceeds u32"))
}

fn bounded_usize(value: Option<i64>, field: &str, max: usize) -> Result<usize, String> {
    let n = bounded_u64(value, field, max as u64)?;
    usize::try_from(n).map_err(|_| format!("CallbackModule.{field} exceeds usize"))
}

#[cfg(test)]
pub(crate) fn fixture_create_workspace_artifact() -> &'static [u8] {
    static ARTIFACT: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    ARTIFACT.get_or_init(|| {
        crate::plugin::tests::build_plugin_afb_bytes(include_bytes!(env!(
            "GENTS_CALLBACK_FIXTURE_CREATE_WORKSPACE_WASM_PATH"
        )))
    })
}

#[cfg(test)]
pub(crate) fn fixture_artifact_is_stub(bytes: &[u8]) -> bool {
    afterburner_afb::Afb::from_bytes(bytes)
        .map(|artifact| {
            artifact
                .precompiled
                .values()
                .all(|module| module.len() <= 16)
        })
        .unwrap_or(true)
}
