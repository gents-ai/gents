//! Shared inference-backend onboarding: local-server detection, live health
//! probing, and the first-launch decision tree from #647.
//!
//! Consumed by the interactive `onboard` command and the `demo` setup, so both
//! agree on what "a local server" and "configure and connect a backend" mean.
//! (`serve` does not call this module; it only prints a string pointing the
//! operator at `onboard` when the default behavior is not runnable.) The
//! decision tree itself ([`plan_backend_onboarding`]) is pure and total —
//! unit-tested without a node or network.

use std::time::Duration;

use serde_json::Value;

/// Common local inference endpoints, probed in priority order:
/// llama-server / LM Studio (8080), Ollama (11434), vLLM (8000).
const LOCAL_BACKEND_CANDIDATES: &[&str] = &[
    "http://127.0.0.1:8080/v1",
    "http://127.0.0.1:11434/v1",
    "http://127.0.0.1:8000/v1",
];

/// A reachable local backend: its base URL and first advertised model id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DetectedBackend {
    pub(crate) url: String,
    pub(crate) model: String,
}

/// A backend already stored in the node, as seen by the onboarding decision.
/// `endpoint` is carried for the caller's reachability probe; the pure decision
/// tree ignores it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfiguredBackend {
    pub(crate) backend_id: String,
    pub(crate) enabled: bool,
    pub(crate) endpoint: Option<String>,
}

/// What first-launch onboarding should do, given the currently-stored backends
/// and whether a local server is live. Encodes #647's tree exactly:
/// stored-single → auto; stored-many → select; none+detected → adopt;
/// none+nothing → offer to launch a local server or configure a remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BackendPlan {
    /// Exactly one usable stored backend — connect to it, no prompt.
    AutoConnect { backend_id: String },
    /// Multiple usable stored backends — the caller must let the user pick.
    Select { backend_ids: Vec<String> },
    /// No usable stored backend, but a local server answered — adopt it.
    AdoptDetected { detected: DetectedBackend },
    /// No usable stored backend and nothing detected — the caller must offer
    /// to launch a local server or configure a remote one.
    OfferLaunchOrRemote,
}

/// The pure #647 decision. `configured` is the stored backend set; a backend
/// only counts when `enabled` (a disabled row is not a connectable choice).
///
/// Health is deliberately NOT consulted here: the stored `probe_status` is not
/// yet trustworthy (#640 — dead endpoints report healthy), so gating on it
/// would hide real backends. Once probes measure truthfully, an
/// `enabled && healthy` filter belongs here.
pub(crate) fn plan_backend_onboarding(
    configured: &[ConfiguredBackend],
    detected: Option<DetectedBackend>,
) -> BackendPlan {
    let usable: Vec<String> = configured
        .iter()
        .filter(|backend| backend.enabled)
        .map(|backend| backend.backend_id.clone())
        .collect();
    match usable.len() {
        0 => match detected {
            Some(detected) => BackendPlan::AdoptDetected { detected },
            None => BackendPlan::OfferLaunchOrRemote,
        },
        1 => BackendPlan::AutoConnect {
            backend_id: usable.into_iter().next().expect("len checked"),
        },
        _ => BackendPlan::Select {
            backend_ids: usable,
        },
    }
}

/// GET `{base}/models`; return the first advertised model id iff the endpoint
/// answers 2xx within the timeout. A real reachability+liveness probe — a dead
/// endpoint yields `None` — unlike a stored `probe_status` flag (#640).
pub(crate) async fn probe_models(base: &str) -> Option<String> {
    let base = base.trim_end_matches('/');
    let response = reqwest::Client::new()
        .get(format!("{base}/models"))
        .timeout(Duration::from_millis(700))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body: Value = response.json().await.ok()?;
    body.pointer("/data/0/id")
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

/// Probe the common local endpoints in priority order; return the first live
/// one, or `None` when no local inference server is reachable.
pub(crate) async fn detect_local_backend() -> Option<DetectedBackend> {
    for url in LOCAL_BACKEND_CANDIDATES {
        if let Some(model) = probe_models(url).await {
            return Some(DetectedBackend {
                url: (*url).to_string(),
                model,
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured(entries: &[(&str, bool)]) -> Vec<ConfiguredBackend> {
        entries
            .iter()
            .map(|(id, enabled)| ConfiguredBackend {
                backend_id: (*id).to_string(),
                enabled: *enabled,
                endpoint: None,
            })
            .collect()
    }

    fn detected() -> DetectedBackend {
        DetectedBackend {
            url: "http://127.0.0.1:11434/v1".to_string(),
            model: "llama3".to_string(),
        }
    }

    #[test]
    fn single_stored_backend_auto_connects() {
        assert_eq!(
            plan_backend_onboarding(&configured(&[("openai", true)]), None),
            BackendPlan::AutoConnect {
                backend_id: "openai".to_string()
            }
        );
    }

    #[test]
    fn multiple_stored_backends_require_selection() {
        assert_eq!(
            plan_backend_onboarding(&configured(&[("openai", true), ("ollama", true)]), None),
            BackendPlan::Select {
                backend_ids: vec!["openai".to_string(), "ollama".to_string()]
            }
        );
    }

    #[test]
    fn disabled_backends_do_not_count_as_usable() {
        // One enabled among disabled rows still auto-connects to the enabled one.
        assert_eq!(
            plan_backend_onboarding(
                &configured(&[("old", false), ("live", true), ("stale", false)]),
                None
            ),
            BackendPlan::AutoConnect {
                backend_id: "live".to_string()
            }
        );
        // All-disabled behaves as no usable backend: fall through to detection.
        assert_eq!(
            plan_backend_onboarding(&configured(&[("old", false)]), Some(detected())),
            BackendPlan::AdoptDetected {
                detected: detected()
            }
        );
    }

    #[test]
    fn no_stored_backend_adopts_a_detected_local_server() {
        assert_eq!(
            plan_backend_onboarding(&[], Some(detected())),
            BackendPlan::AdoptDetected {
                detected: detected()
            }
        );
    }

    #[test]
    fn no_stored_backend_and_nothing_detected_offers_launch_or_remote() {
        assert_eq!(
            plan_backend_onboarding(&[], None),
            BackendPlan::OfferLaunchOrRemote
        );
    }
}
