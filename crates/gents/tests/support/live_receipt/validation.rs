use anyhow::{ensure, Context, Result};
use gents::inference::rendered_request::{CaptureOrderKey, CaptureScope, ProvenanceManifest};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{ExpectedCall, ReceiptRequest};

pub(super) fn rows<'a>(response: &'a Value, collection: &str, bound: usize) -> Result<&'a [Value]> {
    ensure!(
        response
            .get("errors")
            .is_none_or(|value| value.is_null() || value.as_array().is_some_and(Vec::is_empty)),
        "receipt query returned errors"
    );
    let rows = response
        .get("data")
        .and_then(|data| data.get(collection))
        .and_then(Value::as_array)
        .context("receipt query lacks row array")?;
    ensure!(rows.len() <= bound, "receipt row count exceeds bound");
    Ok(rows)
}

pub(super) fn string<'a>(row: &'a Value, field: &str) -> Result<&'a str> {
    row.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("receipt lacks {field}"))
}

pub(super) fn integer(row: &Value, field: &str) -> Result<i64> {
    row.get(field)
        .and_then(Value::as_i64)
        .with_context(|| format!("receipt lacks {field}"))
}

pub(super) fn validate_request(row: &Value, expected: &ReceiptRequest) -> Result<()> {
    for (field, value) in [
        ("request_id", &expected.request_id),
        ("node_did", &expected.node_did),
        ("requester_did", &expected.requester_did),
        ("agent_id", &expected.agent_id),
        ("session_id", &expected.session_id),
        ("lifecycle_state", &expected.lifecycle_state),
    ] {
        ensure!(string(row, field)? == value, "request {field} mismatch");
    }
    string(row, "_docID")?;
    Ok(())
}

pub(super) fn validate_owner(
    row: &Value,
    expected: &ReceiptRequest,
    physical: &str,
    capture: bool,
) -> Result<()> {
    for (field, value) in [
        ("request_id", expected.request_id.as_str()),
        ("node_did", &expected.node_did),
        ("agent_id", &expected.agent_id),
        ("session_id", &expected.session_id),
        ("request_doc_id", physical),
    ] {
        ensure!(string(row, field)? == value, "attempt {field} mismatch");
    }
    if capture {
        ensure!(
            string(row, "requester_did")? == expected.requester_did,
            "capture requester mismatch"
        );
        string(row, "request_commit_cid")?;
    }
    string(row, "_docID")?;
    Ok(())
}

pub(super) fn capture_order(row: &Value) -> Result<CaptureOrderKey> {
    let scope: CaptureScope = string(row, "capture_scope")?.parse()?;
    let turn_index = integer(row, "turn_index")?;
    let attempt = integer(row, "attempt")?;
    ensure!(
        scope.seq > 0 && turn_index >= 0 && attempt >= 0,
        "invalid capture order"
    );
    Ok(CaptureOrderKey {
        scope,
        turn_index,
        attempt,
    })
}

pub(super) fn validate_body(
    row: &Value,
    body: &Value,
    manifest: &ProvenanceManifest,
    target: &ExpectedCall,
) -> Result<Value> {
    target.sampling.validate()?;
    let endpoint = reqwest::Url::parse(&target.endpoint).context("invalid expected endpoint")?;
    ensure!(
        matches!(endpoint.scheme(), "http" | "https")
            && endpoint.host_str().is_some()
            && endpoint.username().is_empty()
            && endpoint.password().is_none()
            && endpoint.query().is_none()
            && endpoint.fragment().is_none()
            && endpoint.path() == "/",
        "expected endpoint must be a credential-free origin"
    );
    ensure!(
        target.route_path.starts_with('/') && !target.route_path.contains(['?', '#']),
        "expected route must be a path"
    );
    ensure!(
        manifest.provider_endpoint.as_deref() == Some(target.endpoint.trim_end_matches('/')),
        "transport endpoint mismatch"
    );
    ensure!(
        manifest.provider_family.as_deref() == Some(target.provider_family.as_str()),
        "provider family mismatch"
    );
    ensure!(
        manifest.provider_route_path_sha256.as_deref()
            == Some(digest(target.route_path.as_bytes()).as_str()),
        "transport route mismatch"
    );
    ensure!(
        string(row, "source")? == target.source && string(row, "model_name")? == target.model,
        "capture source/model mismatch"
    );
    ensure!(
        string(body, "model")? == target.model,
        "decoded model mismatch"
    );
    let cap_field = match target.source.as_str() {
        "openai_responses" => "max_output_tokens",
        "openai_chat_completions" | "claude_cli_subscription" => "max_tokens",
        _ => anyhow::bail!("unsupported expected wire source"),
    };
    ensure!(
        target.output_token_cap > 0 && body[cap_field].as_u64() == Some(target.output_token_cap),
        "output token cap mismatch"
    );
    for other in ["max_tokens", "max_output_tokens", "max_completion_tokens"] {
        ensure!(
            other == cap_field || body.get(other).is_none(),
            "unexpected competing output cap"
        );
    }
    let expected = serde_json::to_value(&target.sampling)?;
    let mut sampling = serde_json::Map::new();
    for field in [
        "temperature",
        "top_p",
        "top_k",
        "seed",
        "min_p",
        "frequency_penalty",
        "presence_penalty",
        "repetition_penalty",
    ] {
        ensure!(
            body.get(field) == expected.get(field),
            "decoded sampling {field} mismatch"
        );
        if let Some(value) = body.get(field) {
            sampling.insert(field.to_owned(), value.clone());
        }
    }
    Ok(
        json!({"backend_id": target.backend_id, "model": target.model,
        "endpoint": target.endpoint, "route_path": target.route_path,
        "provider_family": target.provider_family, "source": target.source,
        "sampling": sampling, "output_token_cap": target.output_token_cap}),
    )
}

pub(super) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
