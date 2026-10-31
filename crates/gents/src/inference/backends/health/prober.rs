use std::sync::Arc;

use chrono::Utc;
use defra_node::EmbeddedNode;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::inference::backends::registry::{
    list_enabled_backends_for_agent, set_backend_probe_status_with_last_probe,
};

use super::oauth::OAuthProbeContext;
use super::probe::{probe_selected_backends, ProbeCycleOutcome, ProbeEnv, ProbeSchedule};
use super::{BackendHealthMap, BackendProberOptions};

pub async fn run_backend_probe_cycle(
    node: Arc<EmbeddedNode>,
    client: &reqwest::Client,
    health_map: &BackendHealthMap,
    options: &BackendProberOptions,
    node_did: &str,
) -> ProbeCycleOutcome {
    run_scheduled_backend_probe_cycle(node, client, health_map, options, node_did, None, None).await
}

pub(super) async fn run_scheduled_backend_probe_cycle(
    node: Arc<EmbeddedNode>,
    client: &reqwest::Client,
    health_map: &BackendHealthMap,
    options: &BackendProberOptions,
    node_did: &str,
    schedule: Option<(&mut ProbeSchedule, bool)>,
    cancel: Option<&CancellationToken>,
) -> ProbeCycleOutcome {
    if cancel.is_some_and(CancellationToken::is_cancelled) {
        return ProbeCycleOutcome::default();
    }
    let backends = match list_enabled_backends_for_agent(node.as_ref(), node_did).await {
        Ok(backends) => backends,
        Err(error) => {
            tracing::warn!(error = %error, "backend probe: could not list backends");
            return ProbeCycleOutcome::default();
        }
    };

    let selected = schedule.map(|(schedule, periodic)| schedule.select(&backends, periodic));
    let now = Utc::now();
    let outcome = probe_selected_backends(
        &node,
        client,
        &backends,
        ProbeEnv {
            now,
            health_map,
            options,
            cancel,
        },
        Some(OAuthProbeContext {
            node: node.clone(),
            node_did,
        }),
        selected.as_ref(),
    )
    .await;

    for backend in backends.iter().filter(|backend| {
        outcome
            .promotable
            .iter()
            .any(|backend_id| backend_id == &backend.backend_id)
    }) {
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            break;
        }
        match set_backend_probe_status_with_last_probe(
            &node,
            &backend.node_did,
            &backend.backend_id,
            "healthy",
            now,
        )
        .await
        {
            Ok(()) => tracing::info!(
                node_did = %backend.node_did,
                backend_id = %backend.backend_id,
                "backend probe: promoted shared document unknown -> healthy"
            ),
            Err(error) => tracing::warn!(
                node_did = %backend.node_did,
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
    node_did: String,
) -> tokio::task::JoinHandle<()> {
    let backend_collection =
        crate::collection_ids(&node, ["InferenceBackend"]).unwrap_or_else(|error| {
            tracing::warn!(%error, "backend prober: probing on the interval only");
            Vec::new()
        });
    let mut changes = node.subscribe_collection_changes(backend_collection);
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
        let mut schedule = ProbeSchedule::default();

        loop {
            let periodic = tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    tracing::debug!("backend prober cancelled");
                    return;
                }
                _ = ticker.tick() => true,
                batch = changes.recv() => {
                    if batch.is_none() {
                        return;
                    }
                    false
                }
            };
            let outcome = run_scheduled_backend_probe_cycle(
                node.clone(),
                &client,
                &health_map,
                &options,
                &node_did,
                Some((&mut schedule, periodic)),
                Some(&cancel),
            )
            .await;
            if !outcome.flipped.is_empty() {
                let _ = health_events_tx.try_send(());
            }
        }
    })
}
