//! Scheduled inference-backend prober (#640).
//!
//! Measures per-runtime reachability of enabled backends and keeps an
//! in-memory [`BackendHealthMap`] that the admission/reconcile layer merges
//! into effective availability. Reachability is observer-relative — each
//! runtime's opinion governs only its own routing — so measured state is
//! deliberately NOT persisted to the fleet-replicated `InferenceBackend`
//! document. The shared document's `probe_status` stays the operator/bootstrap
//! intent knob; the prober's only doc write is the recurring
//! `unknown → healthy` promotion (closing the dead-at-startup gap the
//! startup-only ratchet left open).
//!
//! The state machine mirrors `Proofs/BackendHealth/Transition.lean` exactly
//! and is fenced by the generated `backend_health_cases`: K consecutive
//! failures demote to `Unhealthy` (vetoing routing), a single success
//! promotes back to `Healthy`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use tokio::sync::{mpsc, RwLock};
use tokio_util::sync::CancellationToken;

use crate::backend_registry::{
    list_enabled_backends, set_backend_probe_status_with_last_probe, InferenceBackend,
    UNKNOWN_PROBE_STATUS,
};
use crate::oauth_credential::OAuthRefreshKind;

#[derive(Clone, Debug)]
pub struct BackendProberOptions {
    pub probe_interval: Duration,
    pub probe_timeout: Duration,
    pub failure_threshold_k: u32,
}

impl Default for BackendProberOptions {
    fn default() -> Self {
        Self {
            probe_interval: Duration::from_secs(60),
            probe_timeout: Duration::from_secs(10),
            failure_threshold_k: 3,
        }
    }
}

/// Measured health of one backend as observed by THIS runtime.
/// Mirrors `Proofs.BackendHealth.HealthState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendHealthState {
    Unknown,
    Healthy,
    Degraded,
    Unhealthy,
}

impl BackendHealthState {
    pub fn blocks_routing(self) -> bool {
        matches!(self, Self::Unhealthy)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Unhealthy => "unhealthy",
        }
    }
}

/// Probe outcome vocabulary — mirrors `Proofs.BackendHealth.Event`.
/// `ProbeFail` folds connect failure, non-2xx, and timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeEvent {
    ProbeSuccess,
    ProbeFail,
}

#[derive(Debug, Clone)]
struct BackendHealthEntry {
    state: BackendHealthState,
    failure_count: u32,
    last_probe_at: DateTime<Utc>,
    last_error: Option<String>,
}

/// Public per-backend snapshot — the #631 signal surface (completion retry
/// consults this for fail-fast-vs-backoff) and the metrics overlay source.
#[derive(Debug, Clone)]
pub struct BackendHealthSnapshot {
    pub backend_id: String,
    pub state: BackendHealthState,
    pub failure_count: u32,
    pub last_probe_at: DateTime<Utc>,
    pub last_error: Option<String>,
}

#[derive(Clone, Default)]
pub struct BackendHealthMap {
    inner: Arc<RwLock<HashMap<String, BackendHealthEntry>>>,
}

impl BackendHealthMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn get(&self, backend_id: &str) -> Option<BackendHealthSnapshot> {
        self.inner
            .read()
            .await
            .get(backend_id)
            .map(|entry| snapshot_from_entry(backend_id, entry))
    }

    pub async fn snapshot(&self) -> HashMap<String, BackendHealthSnapshot> {
        self.inner
            .read()
            .await
            .iter()
            .map(|(backend_id, entry)| (backend_id.clone(), snapshot_from_entry(backend_id, entry)))
            .collect()
    }

    pub async fn vetoed_backend_ids(&self) -> HashSet<String> {
        self.inner
            .read()
            .await
            .iter()
            .filter(|(_, entry)| entry.state.blocks_routing())
            .map(|(backend_id, _)| backend_id.clone())
            .collect()
    }

    pub async fn measured_blocks_routing(&self, backend_id: &str) -> bool {
        self.inner
            .read()
            .await
            .get(backend_id)
            .is_some_and(|entry| entry.state.blocks_routing())
    }

    async fn get_model(&self, backend_id: &str) -> (BackendHealthState, u32) {
        self.inner
            .read()
            .await
            .get(backend_id)
            .map(|entry| (entry.state, entry.failure_count))
            .unwrap_or((BackendHealthState::Unknown, 0))
    }

    async fn set_entry(&self, backend_id: String, entry: BackendHealthEntry) {
        self.inner.write().await.insert(backend_id, entry);
    }

    async fn retain_backends(&self, backend_ids: &HashSet<String>) {
        self.inner
            .write()
            .await
            .retain(|backend_id, _| backend_ids.contains(backend_id));
    }

    #[cfg(test)]
    pub(crate) async fn set_for_test(
        &self,
        backend_id: impl Into<String>,
        state: BackendHealthState,
        failure_count: u32,
    ) {
        self.set_entry(
            backend_id.into(),
            BackendHealthEntry {
                state,
                failure_count,
                last_probe_at: Utc::now(),
                last_error: None,
            },
        )
        .await;
    }
}

fn snapshot_from_entry(backend_id: &str, entry: &BackendHealthEntry) -> BackendHealthSnapshot {
    BackendHealthSnapshot {
        backend_id: backend_id.to_string(),
        state: entry.state,
        failure_count: entry.failure_count,
        last_probe_at: entry.last_probe_at,
        last_error: entry.last_error.clone(),
    }
}

/// One transition step — mirrors `Proofs.BackendHealth.step` exactly (fenced
/// by the generated cases in the tests module below).
fn step_backend(
    prev: (BackendHealthState, u32),
    event: ProbeEvent,
    threshold_k: u32,
) -> (BackendHealthState, u32) {
    let threshold_k = threshold_k.max(1);
    match event {
        ProbeEvent::ProbeSuccess => (BackendHealthState::Healthy, 0),
        ProbeEvent::ProbeFail => {
            let n = prev.1.saturating_add(1);
            if n >= threshold_k {
                (BackendHealthState::Unhealthy, n)
            } else {
                (BackendHealthState::Degraded, n)
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct ProbeCycleOutcome {
    pub flipped: Vec<String>,
    pub promotable: Vec<String>,
}

/// Node + principal used to refresh and resolve agent-scoped OAuth credentials
/// during a probe cycle. Refresh and discovery go through the existing
/// OAuth credential owner (`bootstrap_oauth_client` and the bearer it mints);
/// this module never touches tokens directly.
pub struct OAuthProbeContext<'a> {
    pub node: Arc<EmbeddedNode>,
    pub principal_did: &'a str,
}

/// Refresh (if stale) and read the invoking principal's OAuthCredential through
/// the existing credential owner. Returns the credential document needed by
/// `discover_models`' OAuth path; failures keep the classified auth-error copy.
async fn oauth_credential_for_probe(
    context: &OAuthProbeContext<'_>,
    backend: &InferenceBackend,
) -> anyhow::Result<crate::oauth_credential::OAuthCredential> {
    let Some(provider) = backend.provider_kind.oauth_provider() else {
        anyhow::bail!("backend kind has no OAuth provider");
    };
    let (_, credential) = crate::oauth_http::bootstrap_oauth_client(
        context.node.clone(),
        context.principal_did,
        provider,
        oauth_refresh_kind(backend.provider_kind),
        oauth_product(backend.provider_kind),
    )
    .await
    .with_context(|| {
        format!(
            "resolving OAuth credential for backend {} provider {provider}",
            backend.backend_id
        )
    })?;
    tracing::debug!(
        backend_id = %backend.backend_id,
        provider,
        expires_at = %credential.access_token_expires_at.to_rfc3339(),
        "oauth credential probe ok: bearer current through the credential owner"
    );
    Ok(credential)
}

fn oauth_refresh_kind(kind: crate::backend_provider::BackendProviderKind) -> OAuthRefreshKind {
    match kind {
        crate::backend_provider::BackendProviderKind::ChatGptCodex => OAuthRefreshKind::ChatGpt,
        crate::backend_provider::BackendProviderKind::XaiGrokOAuth => OAuthRefreshKind::Xai,
        crate::backend_provider::BackendProviderKind::ClaudeCliSubscription => {
            OAuthRefreshKind::Claude
        }
        _ => unreachable!("only agent-scoped OAuth kinds reach OAuth probing"),
    }
}

fn oauth_product(
    kind: crate::backend_provider::BackendProviderKind,
) -> crate::oauth_credential::OAuthProduct {
    match kind {
        crate::backend_provider::BackendProviderKind::ChatGptCodex => {
            crate::oauth_credential::CHATGPT_OAUTH_PRODUCT
        }
        crate::backend_provider::BackendProviderKind::XaiGrokOAuth => {
            crate::oauth_credential::XAI_OAUTH_PRODUCT
        }
        crate::backend_provider::BackendProviderKind::ClaudeCliSubscription => {
            crate::claude_oauth::CLAUDE_OAUTH_PRODUCT
        }
        _ => unreachable!("only agent-scoped OAuth kinds reach OAuth probing"),
    }
}

/// Discovery timeout for one backend's probe, from the backend's configured
/// discovery timeout (default 10s), floored at 1s.
fn probe_timeout(backend: &InferenceBackend) -> Duration {
    Duration::from_secs(backend.discovery_timeout_secs.unwrap_or(10).max(1) as u64)
}

/// Persist a successful discovery using the registry's atomic scope merge.
/// The credential scope is exact: `None` is the shared-credential scope,
/// `Some(principal)` is that principal's OAuth scope. Failures never write,
/// so a previous catalog and its `observed_at` are preserved.
async fn record_discovered_catalog(
    node: &EmbeddedNode,
    backend: &InferenceBackend,
    principal_did: Option<&str>,
    models: Vec<crate::document_config::AdvertisedModel>,
) {
    let catalog = crate::document_config::BackendModelCatalog {
        agent_did: principal_did.map(str::to_string),
        observed_at: Utc::now().to_rfc3339(),
        models,
    };
    if let Err(error) = crate::backend_registry::record_model_catalog(node, backend, catalog).await
    {
        tracing::warn!(
            agent_did = %backend.agent_did,
            backend_id = %backend.backend_id,
            credential_scope = principal_did.unwrap_or("shared"),
            error = %error,
            "backend probe: discovered models but could not record the catalog; \
             previous observation preserved"
        );
    }
}

pub async fn probe_backends_cycle(
    node: &EmbeddedNode,
    client: &reqwest::Client,
    backends: &[InferenceBackend],
    now: DateTime<Utc>,
    health_map: &BackendHealthMap,
    options: &BackendProberOptions,
    oauth: Option<OAuthProbeContext<'_>>,
) -> ProbeCycleOutcome {
    let mut outcome = ProbeCycleOutcome::default();
    let mut probed_ids = HashSet::new();

    for backend in backends {
        if backend.provider_kind.is_agent_scoped_oauth() {
            let Some(context) = oauth.as_ref() else {
                continue;
            };
            probed_ids.insert(backend.backend_id.clone());

            let (event, error_text) = match oauth_credential_for_probe(context, backend).await {
                Ok(credential) => {
                    let probe_result = tokio::time::timeout(
                        probe_timeout(backend),
                        crate::backend_provider::discover_models(
                            &client,
                            backend.provider_kind,
                            &backend.endpoint,
                            None,
                            Some(&credential),
                        ),
                    )
                    .await;
                    match probe_result {
                        Ok(Ok(models)) => {
                            record_discovered_catalog(
                                context.node.as_ref(),
                                backend,
                                Some(context.principal_did),
                                models,
                            )
                            .await;
                            (ProbeEvent::ProbeSuccess, None)
                        }
                        Ok(Err(error)) => (ProbeEvent::ProbeFail, Some(error.to_string())),
                        Err(_) => (ProbeEvent::ProbeFail, Some("probe timed out".to_string())),
                    }
                }
                Err(error) => (ProbeEvent::ProbeFail, Some(format!("{error:#}"))),
            };
            record_probe_event(
                node,
                backend,
                event,
                error_text,
                now,
                health_map,
                options,
                &mut outcome,
            )
            .await;
            continue;
        }

        probed_ids.insert(backend.backend_id.clone());

        let (event, error_text) = match backend.auth.resolve_api_key() {
            Err(error) => (ProbeEvent::ProbeFail, Some(error.to_string())),
            Ok(api_key) => {
                let probe_result = tokio::time::timeout(
                    probe_timeout(backend),
                    crate::backend_provider::discover_models(
                        client,
                        backend.provider_kind,
                        &backend.endpoint,
                        api_key.as_deref(),
                        None,
                    ),
                )
                .await;

                match probe_result {
                    Ok(Ok(models)) => {
                        record_discovered_catalog(node, backend, None, models).await;
                        (ProbeEvent::ProbeSuccess, None)
                    }
                    Ok(Err(error)) => (ProbeEvent::ProbeFail, Some(error.to_string())),
                    Err(_) => (ProbeEvent::ProbeFail, Some("probe timed out".to_string())),
                }
            }
        };

        record_probe_event(
            node,
            backend,
            event,
            error_text,
            now,
            health_map,
            options,
            &mut outcome,
        )
        .await;
    }

    health_map.retain_backends(&probed_ids).await;
    outcome
}

async fn record_probe_event(
    node: &EmbeddedNode,
    backend: &InferenceBackend,
    event: ProbeEvent,
    error_text: Option<String>,
    now: DateTime<Utc>,
    health_map: &BackendHealthMap,
    options: &BackendProberOptions,
    outcome: &mut ProbeCycleOutcome,
) {
    let previous = health_map.get_model(&backend.backend_id).await;
    let next = step_backend(previous, event, options.failure_threshold_k);
    let veto_flipped = previous.0.blocks_routing() != next.0.blocks_routing();

    if veto_flipped {
        tracing::warn!(
            backend_id = %backend.backend_id,
            endpoint = %backend.endpoint,
            previous_state = %previous.0.as_str(),
            next_state = %next.0.as_str(),
            failure_count = next.1,
            error = error_text.as_deref().unwrap_or(""),
            "backend probe: measured health crossed the routing threshold"
        );
        outcome.flipped.push(backend.backend_id.clone());
    } else if event == ProbeEvent::ProbeFail {
        tracing::debug!(
            backend_id = %backend.backend_id,
            endpoint = %backend.endpoint,
            state = %next.0.as_str(),
            failure_count = next.1,
            error = error_text.as_deref().unwrap_or(""),
            "backend probe failed"
        );
    }

    if event == ProbeEvent::ProbeSuccess {
        match crate::backend_registry::lookup_backend_observation(
            node,
            &backend.agent_did,
            &backend.backend_id,
        )
        .await
        {
            Ok(Some(observation))
                if observation
                    .probe_status
                    .as_deref()
                    .unwrap_or(UNKNOWN_PROBE_STATUS)
                    == UNKNOWN_PROBE_STATUS =>
            {
                outcome.promotable.push(backend.backend_id.clone());
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(agent_did = %backend.agent_did,
                backend_id = %backend.backend_id, %error,
                "backend probe: could not read promotion observation"),
        }
    }

    health_map
        .set_entry(
            backend.backend_id.clone(),
            BackendHealthEntry {
                state: next.0,
                failure_count: next.1,
                last_probe_at: now,
                last_error: error_text,
            },
        )
        .await;
}

pub async fn run_backend_probe_cycle(
    node: Arc<EmbeddedNode>,
    client: &reqwest::Client,
    health_map: &BackendHealthMap,
    options: &BackendProberOptions,
    principal_did: &str,
) -> ProbeCycleOutcome {
    let backends = match list_enabled_backends(node.as_ref()).await {
        Ok(backends) => backends
            .into_iter()
            .filter(|backend| backend.agent_did == principal_did)
            .collect::<Vec<_>>(),
        Err(error) => {
            tracing::warn!(error = %error, "backend probe: could not list backends");
            return ProbeCycleOutcome::default();
        }
    };

    let now = Utc::now();
    let outcome = probe_backends_cycle(
        node.as_ref(),
        client,
        &backends,
        now,
        health_map,
        options,
        Some(OAuthProbeContext {
            node: node.clone(),
            principal_did,
        }),
    )
    .await;

    for backend in backends.iter().filter(|backend| {
        outcome
            .promotable
            .iter()
            .any(|backend_id| backend_id == &backend.backend_id)
    }) {
        match set_backend_probe_status_with_last_probe(
            node.as_ref(),
            &backend.agent_did,
            &backend.backend_id,
            "healthy",
            now,
        )
        .await
        {
            Ok(()) => tracing::info!(
                agent_did = %backend.agent_did,
                backend_id = %backend.backend_id,
                "backend probe: promoted shared document unknown -> healthy"
            ),
            Err(error) => tracing::warn!(
                agent_did = %backend.agent_did,
                backend_id = %backend.backend_id,
                error = %error,
                "backend probe: reachable but failed to persist promotion"
            ),
        }
    }

    outcome
}

pub fn spawn_backend_prober(
    node: Arc<EmbeddedNode>,
    health_map: BackendHealthMap,
    options: BackendProberOptions,
    health_events_tx: mpsc::Sender<()>,
    cancel: CancellationToken,
    principal_did: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let client = match reqwest::Client::builder()
            .timeout(options.probe_timeout)
            .build()
        {
            Ok(client) => client,
            Err(error) => {
                tracing::warn!(error = %error, "backend prober: could not build HTTP client");
                return;
            }
        };

        let mut ticker = tokio::time::interval(options.probe_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    tracing::debug!("backend prober cancelled");
                    return;
                }
                _ = ticker.tick() => {
                    let outcome = run_backend_probe_cycle(
                        node.clone(),
                        &client,
                        &health_map,
                        &options,
                        &principal_did,
                    )
                    .await;
                    if !outcome.flipped.is_empty() {
                        let _ = health_events_tx.try_send(());
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use axum::{routing::get, Json, Router};
    use tokio::sync::oneshot;

    use super::*;
    use crate::backend_registry::DEFAULT_MAX_QUEUE_DEPTH;
    use crate::lean_vocab_test::lean_backend_health_cases;
    use crate::oauth_credential::test_support::{
        seed_credential, seed_credential_with_refresh_token, test_node,
    };

    fn state_from_lean(name: &str, state: &str) -> BackendHealthState {
        match state {
            "unknown" => BackendHealthState::Unknown,
            "healthy" => BackendHealthState::Healthy,
            "degraded" => BackendHealthState::Degraded,
            "unhealthy" => BackendHealthState::Unhealthy,
            other => panic!("Lean backend health case {name} produced unknown state {other:?}"),
        }
    }

    #[test]
    fn generated_backend_health_cases_match_prober_transitions() {
        let cases = lean_backend_health_cases();
        assert!(
            !cases.is_empty(),
            "Lean must emit at least one backend health case"
        );

        for case in cases {
            let start = (
                state_from_lean(&case.name, &case.start_state),
                u32::try_from(case.start_count).unwrap(),
            );
            let event = match case.event.as_str() {
                "probeSuccess" => ProbeEvent::ProbeSuccess,
                "probeFail" => ProbeEvent::ProbeFail,
                other => panic!(
                    "Lean backend health case {} produced unknown event {other:?}",
                    case.name
                ),
            };

            let (next_state, next_count) =
                step_backend(start, event, u32::try_from(case.threshold_k).unwrap());

            assert_eq!(
                next_state,
                state_from_lean(&case.name, &case.next_state),
                "Lean backend health case {} must match next_state",
                case.name
            );
            assert_eq!(
                next_count as usize, case.next_count,
                "Lean backend health case {} must match next_count",
                case.name
            );
            assert_eq!(
                next_state.blocks_routing(),
                case.blocks_routing,
                "Lean backend health case {} must match the blocksRouting projection",
                case.name
            );
        }
    }

    /// Minimal /v1/models responder on a real socket. Serving on the ambient
    /// Tokio runtime keeps the probe client and responder under one scheduler;
    /// an independently scheduled blocking thread made these tests flaky under
    /// full-suite CPU contention (#743).
    struct ModelsListener {
        port: u16,
        shutdown: Option<oneshot::Sender<()>>,
        handle: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
    }

    impl ModelsListener {
        fn start() -> Self {
            let listener =
                std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind models listener");
            listener
                .set_nonblocking(true)
                .expect("set models listener nonblocking");
            let port = listener.local_addr().expect("local addr").port();
            let listener =
                tokio::net::TcpListener::from_std(listener).expect("build async models listener");
            let app = Router::new().route(
                "/v1/models",
                get(|| async { Json(serde_json::json!({"data": [{"id": "test-model"}]})) }),
            );
            let (shutdown_tx, shutdown_rx) = oneshot::channel();
            let handle = tokio::spawn(async move {
                axum::serve(listener, app)
                    .with_graceful_shutdown(async move {
                        let _ = shutdown_rx.await;
                    })
                    .await
            });
            Self {
                port,
                shutdown: Some(shutdown_tx),
                handle: Some(handle),
            }
        }

        fn endpoint(&self) -> String {
            format!("http://127.0.0.1:{}/v1", self.port)
        }

        async fn shutdown(mut self) {
            if let Some(shutdown) = self.shutdown.take() {
                let _ = shutdown.send(());
            }
            if let Some(handle) = self.handle.take() {
                handle
                    .await
                    .expect("join models listener task")
                    .expect("serve models listener");
            }
        }
    }

    impl Drop for ModelsListener {
        fn drop(&mut self) {
            if let Some(shutdown) = self.shutdown.take() {
                let _ = shutdown.send(());
            }
        }
    }

    fn backend(backend_id: &str, endpoint: String) -> InferenceBackend {
        InferenceBackend {
            agent_did: "did:key:backend-owner".to_string(),
            backend_id: backend_id.to_string(),
            name: backend_id.to_string(),
            provider_kind: crate::backend_provider::BackendProviderKind::OpenAiCompatible,
            openai_wire_api: None,
            endpoint,
            auth: crate::document_config::BackendAuth::Unauthenticated,
            connect_timeout_secs: None,
            discovery_timeout_secs: None,
            max_concurrent: Some(1),
            max_queue_depth: Some(DEFAULT_MAX_QUEUE_DEPTH),
            enabled: true,
            tags: Vec::new(),
        }
    }

    async fn seed_backend_observation(
        node: &EmbeddedNode,
        backend: &InferenceBackend,
        status: &str,
    ) {
        use crate::config_client::{
            ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan,
        };
        let value = serde_json::to_value(backend).unwrap();
        let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
            collection: crate::Collection::InferenceBackend,
            add: value.clone(),
            update: value,
        }])
        .unwrap();
        ConfigAccess::transact_local(node, None, "test.backend.seed", |txn| {
            let plan = &plan;
            Box::pin(async move { crate::config_client::apply_desired_state_plan(txn, plan).await })
        })
        .await
        .unwrap();
        crate::backend_registry::set_backend_probe_status(
            node,
            &backend.agent_did,
            &backend.backend_id,
            status,
        )
        .await
        .unwrap();
    }

    fn probe_options() -> BackendProberOptions {
        BackendProberOptions {
            probe_interval: Duration::from_millis(50),
            probe_timeout: Duration::from_secs(2),
            failure_threshold_k: 3,
        }
    }

    #[tokio::test]
    async fn cycle_reproduces_fleet_evidence_dead_backend_demotes_and_recovers() {
        let options = probe_options();
        let client = reqwest::Client::builder()
            .timeout(options.probe_timeout)
            .build()
            .unwrap();
        let health_map = BackendHealthMap::new();
        let node = Arc::new(test_node().await);

        // Healthy while the endpoint answers.
        let listener = ModelsListener::start();
        let endpoint = listener.endpoint();
        let backends = vec![backend("spark", endpoint.clone())];
        let now = Utc::now();
        let outcome =
            probe_backends_cycle(&node, &client, &backends, now, &health_map, &options, None).await;
        assert!(outcome.flipped.is_empty());
        let snap = health_map.get("spark").await.expect("entry after probe");
        assert_eq!(snap.state, BackendHealthState::Healthy);
        assert_eq!(snap.last_probe_at, now);
        assert!(!health_map.measured_blocks_routing("spark").await);

        // Endpoint goes hard down (connect refused): the fleet-evidence
        // regime where probe_status stayed a stored constant for 16h.
        listener.shutdown().await;
        for cycle in 1..=3u32 {
            let cycle_now = Utc::now();
            let outcome = probe_backends_cycle(
                &node,
                &client,
                &backends,
                cycle_now,
                &health_map,
                &options,
                None,
            )
            .await;
            let snap = health_map.get("spark").await.expect("entry");
            assert_eq!(snap.failure_count, cycle, "consecutive failures accumulate");
            assert_eq!(
                snap.last_probe_at, cycle_now,
                "every attempt stamps last_probe"
            );
            if cycle < 3 {
                assert_eq!(snap.state, BackendHealthState::Degraded);
                assert!(outcome.flipped.is_empty(), "no veto below the K threshold");
                assert!(!health_map.measured_blocks_routing("spark").await);
            } else {
                assert_eq!(snap.state, BackendHealthState::Unhealthy);
                assert_eq!(outcome.flipped, vec!["spark".to_string()]);
                assert!(health_map.measured_blocks_routing("spark").await);
                assert!(snap.last_error.is_some(), "failure detail retained");
            }
        }

        // Backend recovers on a fresh port: one success re-promotes.
        let recovered = ModelsListener::start();
        let backends = vec![backend("spark", recovered.endpoint())];
        let outcome = probe_backends_cycle(
            &node,
            &client,
            &backends,
            Utc::now(),
            &health_map,
            &options,
            None,
        )
        .await;
        assert_eq!(
            outcome.flipped,
            vec!["spark".to_string()],
            "recovery flips the veto back"
        );
        let snap = health_map.get("spark").await.expect("entry");
        assert_eq!(snap.state, BackendHealthState::Healthy);
        assert_eq!(snap.failure_count, 0);
        assert!(snap.last_error.is_none());
        assert!(!health_map.measured_blocks_routing("spark").await);
    }

    #[tokio::test]
    async fn cycle_marks_reachable_unknown_backends_promotable() {
        let options = probe_options();
        let client = reqwest::Client::new();
        let health_map = BackendHealthMap::new();
        let node = Arc::new(test_node().await);
        let listener = ModelsListener::start();

        let backends = vec![backend("late-arrival", listener.endpoint())];
        seed_backend_observation(&node, &backends[0], "unknown").await;
        let outcome = probe_backends_cycle(
            &node,
            &client,
            &backends,
            Utc::now(),
            &health_map,
            &options,
            None,
        )
        .await;
        assert_eq!(outcome.promotable, vec!["late-arrival".to_string()]);

        // Already-promoted docs are not re-written.
        let backends = vec![backend("late-arrival", listener.endpoint())];
        seed_backend_observation(&node, &backends[0], "healthy").await;
        let outcome = probe_backends_cycle(
            &node,
            &client,
            &backends,
            Utc::now(),
            &health_map,
            &options,
            None,
        )
        .await;
        assert!(outcome.promotable.is_empty());

        // Observe the current probe owner persisting a successful promotion.
        seed_backend_observation(&node, &backends[0], "unknown").await;
        let outcome = run_backend_probe_cycle(
            node.clone(),
            &client,
            &health_map,
            &options,
            "did:key:backend-owner",
        )
        .await;
        assert_eq!(outcome.promotable, vec!["late-arrival".to_string()]);

        let document = node
            .execute(
                r#"{ InferenceBackend(filter: { backend_id: { _eq: "late-arrival" } }) { probe_status last_probe } }"#,
            )
            .await;
        assert!(!document.has_errors(), "{:?}", document.errors);
        let rows = document
            .data
            .as_ref()
            .and_then(|data| data.get("InferenceBackend"))
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(
            rows[0]["probe_status"], "healthy",
            "the probe owner must persist its promotion"
        );
        assert!(
            !rows[0]["last_probe"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
            "promotion must stamp last_probe: {rows:?}"
        );
    }

    #[tokio::test]
    async fn runtime_probe_cycle_scopes_same_named_backends_to_its_principal() {
        let node = Arc::new(test_node().await);
        let listener = ModelsListener::start();
        let local = backend("same", listener.endpoint());
        let mut foreign = backend("same", "http://127.0.0.1:1/v1".into());
        foreign.agent_did = "did:key:foreign-owner".into();
        seed_backend_observation(&node, &local, "unknown").await;
        seed_backend_observation(&node, &foreign, "unknown").await;
        let health = BackendHealthMap::new();
        let outcome = run_backend_probe_cycle(
            node.clone(),
            &reqwest::Client::new(),
            &health,
            &probe_options(),
            &local.agent_did,
        )
        .await;
        assert_eq!(outcome.promotable, vec!["same"]);
        assert_eq!(
            health.get("same").await.unwrap().state,
            BackendHealthState::Healthy
        );
        let foreign_observation = crate::backend_registry::lookup_backend_observation(
            &node,
            &foreign.agent_did,
            &foreign.backend_id,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(foreign_observation.probe_status.as_deref(), Some("unknown"));
        assert!(foreign_observation.catalogs.is_empty());
    }

    #[tokio::test]
    async fn cycle_never_probes_or_demotes_chatgpt_codex_backends() {
        let options = probe_options();
        let client = reqwest::Client::new();
        let health_map = BackendHealthMap::new();
        let node = Arc::new(test_node().await);

        // Dead endpoint, but ChatGPT-Codex: OAuthCredential is agent-scoped,
        // so the runtime-level prober must leave it alone entirely.
        let mut codex = backend("codex", "http://127.0.0.1:1/v1".to_string());
        codex.provider_kind = crate::backend_provider::BackendProviderKind::ChatGptCodex;
        codex.auth = crate::document_config::BackendAuth::PrincipalOAuth;
        let outcome = probe_backends_cycle(
            &node,
            &client,
            std::slice::from_ref(&codex),
            Utc::now(),
            &health_map,
            &options,
            None,
        )
        .await;
        assert!(outcome.flipped.is_empty());
        assert!(health_map.get("codex").await.is_none(), "no measured entry");
        assert!(!health_map.measured_blocks_routing("codex").await);
    }

    fn claude_backend() -> InferenceBackend {
        let mut claude = backend(
            "claude",
            crate::claude_subscription::DEFAULT_BACKEND_ENDPOINT.to_string(),
        );
        claude.provider_kind = crate::backend_provider::BackendProviderKind::ClaudeCliSubscription;
        claude.auth = crate::document_config::BackendAuth::PrincipalOAuth;
        claude
    }

    fn oauth_backend(
        kind: crate::backend_provider::BackendProviderKind,
        id: &str,
        endpoint: &str,
    ) -> InferenceBackend {
        let mut backend = backend(id, endpoint.to_string());
        backend.provider_kind = kind;
        backend.auth = crate::document_config::BackendAuth::PrincipalOAuth;
        backend
    }

    #[tokio::test]
    async fn oauth_kinds_probe_the_credential_document_fresh_is_healthy_and_promotes() {
        let node = Arc::new(test_node().await);
        let did = "did:key:z6MkProbe";
        seed_credential(
            &node,
            did,
            crate::claude_oauth::CLAUDE_OAUTH_PROVIDER,
            Utc::now() + chrono::Duration::hours(8),
        )
        .await;
        let (options, client, health_map) = (
            probe_options(),
            reqwest::Client::new(),
            BackendHealthMap::new(),
        );
        let mut claude = claude_backend();
        claude.agent_did = did.to_string();
        seed_backend_observation(&node, &claude, "unknown").await;
        let outcome = probe_backends_cycle(
            &node,
            &client,
            std::slice::from_ref(&claude),
            Utc::now(),
            &health_map,
            &options,
            Some(OAuthProbeContext {
                node: node.clone(),
                principal_did: did,
            }),
        )
        .await;
        assert_eq!(outcome.promotable, vec!["claude".to_string()]);
        let snap = health_map.get("claude").await.expect("entry");
        assert_eq!(snap.state, BackendHealthState::Healthy);
        assert!(snap.last_error.is_none());
    }

    #[tokio::test]
    async fn oauth_kinds_stale_credential_with_refresh_token_stays_healthy_and_promotes() {
        let node = Arc::new(test_node().await);
        let did = "did:key:z6MkProbe";
        seed_credential(
            &node,
            did,
            crate::xai_grok_oauth::XAI_OAUTH_PROVIDER,
            Utc::now() - chrono::Duration::minutes(1),
        )
        .await;
        let (options, client, health_map) = (
            probe_options(),
            reqwest::Client::new(),
            BackendHealthMap::new(),
        );
        let mut grok = oauth_backend(
            crate::backend_provider::BackendProviderKind::XaiGrokOAuth,
            "grok",
            "https://cli-chat-proxy.grok.com/v1",
        );
        grok.agent_did = did.to_string();
        seed_backend_observation(&node, &grok, "unknown").await;
        for _ in 0..3 {
            let outcome = probe_backends_cycle(
                &node,
                &client,
                std::slice::from_ref(&grok),
                Utc::now(),
                &health_map,
                &options,
                Some(OAuthProbeContext {
                    node: node.clone(),
                    principal_did: did,
                }),
            )
            .await;
            assert!(outcome.flipped.is_empty());
            assert_eq!(outcome.promotable, vec!["grok".to_string()]);
            let snap = health_map.get("grok").await.expect("entry");
            assert_eq!(snap.state, BackendHealthState::Healthy);
            assert_eq!(snap.failure_count, 0);
            assert!(snap.last_error.is_none(), "{:?}", snap.last_error);
        }
        assert!(health_map.vetoed_backend_ids().await.is_empty());
    }

    #[tokio::test]
    async fn oauth_kinds_stale_credential_without_refresh_token_still_demotes_after_k() {
        let node = Arc::new(test_node().await);
        let did = "did:key:z6MkProbe";
        seed_credential_with_refresh_token(
            &node,
            did,
            crate::xai_grok_oauth::XAI_OAUTH_PROVIDER,
            Utc::now() - chrono::Duration::minutes(1),
            "",
        )
        .await;
        let (options, client, health_map) = (
            probe_options(),
            reqwest::Client::new(),
            BackendHealthMap::new(),
        );
        let grok = oauth_backend(
            crate::backend_provider::BackendProviderKind::XaiGrokOAuth,
            "grok",
            "https://cli-chat-proxy.grok.com/v1",
        );
        for cycle in 1..=3u32 {
            let outcome = probe_backends_cycle(
                &node,
                &client,
                std::slice::from_ref(&grok),
                Utc::now(),
                &health_map,
                &options,
                Some(OAuthProbeContext {
                    node: node.clone(),
                    principal_did: did,
                }),
            )
            .await;
            assert!(outcome.promotable.is_empty());
            let snap = health_map.get("grok").await.expect("entry");
            assert_eq!(snap.failure_count, cycle);
            let err = snap.last_error.clone().unwrap_or_default();
            assert!(err.contains("missing refresh_token"), "{err}");
            if cycle == 3 {
                assert_eq!(snap.state, BackendHealthState::Unhealthy);
            }
        }
    }

    #[tokio::test]
    async fn oauth_kinds_missing_credential_fails_with_login_hint() {
        let node = Arc::new(test_node().await);
        let (options, client, health_map) = (
            probe_options(),
            reqwest::Client::new(),
            BackendHealthMap::new(),
        );
        let codex = oauth_backend(
            crate::backend_provider::BackendProviderKind::ChatGptCodex,
            "codex",
            "https://chatgpt.com/backend-api/codex",
        );
        probe_backends_cycle(
            &node,
            &client,
            std::slice::from_ref(&codex),
            Utc::now(),
            &health_map,
            &options,
            Some(OAuthProbeContext {
                node: node.clone(),
                principal_did: "did:key:z6MkNobody",
            }),
        )
        .await;
        let snap = health_map.get("codex").await.expect("entry");
        assert_eq!(snap.state, BackendHealthState::Degraded);
        assert!(snap
            .last_error
            .clone()
            .unwrap_or_default()
            .contains("gents codex-login --agent-did did:key:z6MkNobody"));
    }

    #[tokio::test]
    async fn oauth_kinds_are_skipped_without_a_probe_context() {
        let (options, client, health_map) = (
            probe_options(),
            reqwest::Client::new(),
            BackendHealthMap::new(),
        );
        let node = Arc::new(test_node().await);
        let outcome = probe_backends_cycle(
            &node,
            &client,
            std::slice::from_ref(&claude_backend()),
            Utc::now(),
            &health_map,
            &options,
            None,
        )
        .await;
        assert!(outcome.promotable.is_empty());
        assert!(
            health_map.get("claude").await.is_none(),
            "no measurement without a node"
        );
    }

    #[test]
    fn provider_kind_oauth_provider_names() {
        use crate::backend_provider::BackendProviderKind as K;
        assert_eq!(
            K::ClaudeCliSubscription.oauth_provider(),
            Some("claude-subscription")
        );
        assert_eq!(K::XaiGrokOAuth.oauth_provider(), Some("xai-oauth"));
        assert_eq!(K::ChatGptCodex.oauth_provider(), Some("chatgpt-codex"));
        assert_eq!(K::OpenAiCompatible.oauth_provider(), None);
        assert!(K::ClaudeCliSubscription.is_agent_scoped_oauth());
    }

    #[tokio::test]
    async fn cycle_drops_entries_for_backends_no_longer_enabled() {
        let options = probe_options();
        let client = reqwest::Client::new();
        let health_map = BackendHealthMap::new();
        health_map
            .set_for_test("retired", BackendHealthState::Unhealthy, 5)
            .await;

        let node = Arc::new(test_node().await);
        let listener = ModelsListener::start();
        let backends = vec![backend("current", listener.endpoint())];
        probe_backends_cycle(
            &node,
            &client,
            &backends,
            Utc::now(),
            &health_map,
            &options,
            None,
        )
        .await;

        assert!(health_map.get("retired").await.is_none());
        assert!(health_map.get("current").await.is_some());
    }
}
