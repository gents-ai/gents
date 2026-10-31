//! Durable, sanitized evidence exported before a live fixture removes its node.

#[path = "live_receipt/artifact.rs"]
mod artifact;
#[path = "live_receipt/collect.rs"]
mod collect;
#[cfg(test)]
#[path = "live_receipt/tests.rs"]
mod tests;
#[path = "live_receipt/validation.rs"]
mod validation;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};
use gents::documents::{InferenceBackend, InferenceProfile, InferenceSampling};
use gents::store::config_client::ConfigAccess;
use gents::{BackendProviderKind, OpenAiWireApi};
use serde_json::json;

use artifact::{publish_receipt, validate_directory, validate_scenario};
use collect::{collect_request, validate_session_request_sets};

const ARTIFACT_DIR_ENV: &str = "GENTS_LIVE_ARTIFACT_DIR";

/// Call before starting inference so output errors cost no provider work.
pub fn required_artifact_dir() -> Result<PathBuf> {
    let path = std::env::var_os(ARTIFACT_DIR_ENV)
        .map(PathBuf::from)
        .context("GENTS_LIVE_ARTIFACT_DIR must name an existing private directory")?;
    validate_directory(&path)?;
    Ok(path)
}

#[derive(Clone, Debug)]
pub struct ExpectedCall {
    pub backend_id: String,
    pub model: String,
    /// Transport origin, without a path, query, credentials, or fragment.
    pub endpoint: String,
    pub route_path: String,
    pub provider_family: String,
    pub source: String,
    pub sampling: InferenceSampling,
    pub output_token_cap: u64,
}

impl ExpectedCall {
    /// Derive the wire expectation from authored documents, never live DB state.
    pub fn openai_chat(
        backend: &InferenceBackend,
        profile: &InferenceProfile,
        sampling: &InferenceSampling,
    ) -> Result<Self> {
        ensure!(
            backend.provider_kind == BackendProviderKind::OpenAiCompatible
                && OpenAiWireApi::effective_for_provider(
                    backend.provider_kind,
                    backend.openai_wire_api
                ) == OpenAiWireApi::ChatCompletions,
            "live receipt target must use OpenAiCompatible chat completions"
        );
        for identity in [
            &backend.node_did,
            &backend.backend_id,
            &profile.profile_id,
            &profile.model_name,
            &sampling.sampling_id,
        ] {
            ensure!(
                !identity.trim().is_empty(),
                "authored target has a blank identity"
            );
        }
        ensure!(
            profile.node_did == backend.node_did && sampling.node_did == backend.node_did,
            "authored target node references disagree"
        );
        ensure!(
            profile.backend_id == backend.backend_id,
            "authored profile backend reference disagrees"
        );
        ensure!(
            profile.sampling_id.as_deref() == Some(sampling.sampling_id.as_str()),
            "authored profile sampling reference disagrees"
        );
        profile.validate()?;
        sampling.validate()?;
        let cap = profile
            .max_output_tokens
            .filter(|cap| *cap > 0)
            .context("live receipt target requires explicit positive max_output_tokens")?;
        let endpoint =
            reqwest::Url::parse(&backend.endpoint).context("invalid authored endpoint")?;
        ensure!(
            matches!(endpoint.scheme(), "http" | "https")
                && endpoint.host_str().is_some()
                && endpoint.username().is_empty()
                && endpoint.password().is_none()
                && endpoint.query().is_none()
                && endpoint.fragment().is_none(),
            "authored endpoint must be credential-free HTTP without query or fragment"
        );
        Ok(Self {
            backend_id: backend.backend_id.clone(),
            model: profile.model_name.clone(),
            endpoint: endpoint.origin().ascii_serialization(),
            route_path: format!("{}/chat/completions", endpoint.path().trim_end_matches('/')),
            provider_family: "OpenAiCompatible".into(),
            source: "openai_chat_completions".into(),
            sampling: sampling.clone(),
            output_token_cap: u64::try_from(cap)?,
        })
    }
}

#[derive(Clone, Debug)]
pub struct ReceiptRequest {
    pub node_did: String,
    pub requester_did: String,
    pub agent_id: String,
    pub session_id: String,
    pub request_id: String,
    pub request_doc_id: Option<String>,
    pub lifecycle_state: String,
    pub ordinary: ExpectedCall,
    pub summary: Option<ExpectedCall>,
    pub require_summary: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct ReceiptLimits {
    pub max_requests: usize,
    pub max_attempts_per_request: usize,
    pub max_decoded_bytes_per_capture: usize,
    pub max_artifact_bytes: usize,
}

impl Default for ReceiptLimits {
    fn default() -> Self {
        Self {
            max_requests: 8,
            max_attempts_per_request: 64,
            max_decoded_bytes_per_capture: 8 * 1024 * 1024,
            max_artifact_bytes: 2 * 1024 * 1024,
        }
    }
}

/// `private_dir` must already exist outside the checkout with owner-only access.
/// The caller supplies authored expectations, never expectations read back from
/// the running node. Every request is validated before any receipt is published.
pub async fn export_live_receipts(
    access: &ConfigAccess,
    private_dir: &Path,
    scenario: &str,
    requests: &[ReceiptRequest],
    limits: ReceiptLimits,
) -> Result<PathBuf> {
    validate_directory(private_dir)?;
    validate_scenario(scenario)?;
    ensure!(
        !requests.is_empty() && requests.len() <= limits.max_requests,
        "receipt request count is outside its bound"
    );
    ensure!(
        limits.max_attempts_per_request > 0
            && limits.max_attempts_per_request < 4096
            && limits.max_requests < 4096
            && limits.max_decoded_bytes_per_capture > 0
            && limits.max_artifact_bytes > 0,
        "invalid receipt limits"
    );
    let mut identities = BTreeSet::new();
    for expected in requests {
        ensure!(
            identities.insert((&expected.node_did, &expected.request_id)),
            "duplicate requested receipt identity"
        );
    }
    let mut receipts = Vec::new();
    for expected in requests {
        receipts.push(collect_request(access, expected, limits).await?);
    }
    // Read the complete session set last: completion can spawn a sibling while
    // earlier capture evidence is being read.
    validate_session_request_sets(access, requests, limits.max_requests).await?;
    let bytes = serde_json::to_vec_pretty(&json!({
        "receipt_version": 1,
        "requests": receipts,
    }))?;
    ensure!(
        bytes.len() <= limits.max_artifact_bytes,
        "receipt artifact exceeds byte bound"
    );
    publish_receipt(private_dir, scenario, &bytes)
}
