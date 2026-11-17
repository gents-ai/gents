//! A standing provider refusal, derived from the durable evidence of the calls
//! that hit it.
//!
//! Diagnostics only: nothing here switches an account, retries, or changes a
//! route. It answers one question the node could not answer before — is this
//! account out of quota, and what did the provider say — and it answers it from
//! evidence rather than from a percentage, so an unknown stays unknown.
//!
//! ## Where the state comes from, and what it cannot say
//!
//! The evidence is `InferenceCall`: `failure_reason` (which already carries the
//! classifier's `Display` text via `persisted_failure_reason`), `call_state`,
//! `provider_total_tokens` and the call's timestamp. A refusal stands until its
//! stated reset or a later **completed** call; a newer refusal replaces an older
//! one, and an older success does not clear a newer refusal — the rules Lean
//! `InferenceCall.Usage` states and this module replays.
//!
//! `InferenceCall` carries no account identity, so the window query is scoped by
//! backend/model/profile and reports **partial** scope: it is what this node
//! called through this account's backends, not the provider's whole account
//! view. Legacy calls recorded before the binding are left unknown rather than
//! attributed retroactively.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::usage_observation::UsageRefusalView;

/// How far back the evidence window reaches. An analytical choice, not a
/// provider fact: a refusal whose reset is unknown is looked for in this window
/// and the view says the window is analytical.
pub const REFUSAL_EVIDENCE_WINDOW_HOURS: i64 = 24;

/// What one call in the window says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallEvidence {
    pub at: DateTime<Utc>,
    /// `"completed"`, `"failed"` or `"queued"`.
    pub call_state: String,
    /// The persisted failure text, which the classifier can read again.
    pub failure_reason: Option<String>,
}

/// Whether the evidence establishes a standing refusal, and as which view.
///
/// `completed_at` is the latest completion in the window, if any. The rules, in
/// the order Lean states them: a refusal with a stated reset stops standing at
/// that reset; a completion strictly after the refusal clears it; otherwise the
/// latest refusal stands.
pub fn standing_refusal(
    calls: &[CallEvidence],
    now: DateTime<Utc>,
    consumed_by_node: Option<u64>,
    detail_limit: usize,
) -> Option<UsageRefusalView> {
    if calls.is_empty() {
        return None;
    }
    // The latest refusal: the newest call whose failure text classifies as a
    // usage limit, whether or not a status marker survived into the text.
    let refusal = calls
        .iter()
        .filter(|call| call.call_state == "failed")
        .filter_map(|call| {
            let text = call.failure_reason.as_deref()?;
            let limit = gents_loop::provider_limit::classify_provider_limit(text, call.at)?;
            match limit {
                gents_loop::provider_limit::ProviderLimit::UsageExhausted(usage) => {
                    Some((call.at, usage))
                }
                gents_loop::provider_limit::ProviderLimit::Throttled { .. } => None,
            }
        })
        .max_by_key(|(at, _)| *at)?;

    let (observed_at, usage) = refusal;
    // A stated reset ends it.
    if let Some(reset) = usage.resets_at {
        if now >= reset {
            return None;
        }
    }
    // A later completion clears it; an earlier one does not.
    let cleared = calls
        .iter()
        .any(|call| call.call_state == "completed" && call.at > observed_at);
    if cleared {
        return None;
    }

    Some(UsageRefusalView {
        at: observed_at.to_rfc3339(),
        window: usage.window,
        used: usage.used,
        cap: usage.cap,
        resets_at: usage.resets_at.map(|at| at.to_rfc3339()),
        consumed_by_node,
        detail: bounded_detail(&usage.detail, detail_limit),
    })
}

/// The standing refusal for one backend, read from the durable evidence of the
/// calls made through it.
///
/// Diagnostics only, and **partial by construction**: `InferenceCall` carries no
/// account identity, so this asks for the calls this backend made in the
/// evidence window and reports them as this node's evidence, not the provider's
/// whole account view. A read that fails says `None` — a limitation of the
/// diagnostics must not look like a refusal.
pub async fn refusal_for_backend(
    access: &gents_store::config_client::ConfigAccess,
    backend: &gents_documents::inference_backend::InferenceBackend,
    now: DateTime<Utc>,
) -> Option<UsageRefusalView> {
    let since = evidence_since(now);
    let backend_id = gents_store::graphql::escape_graphql_string(&backend.backend_id);
    let since_text = gents_store::graphql::escape_graphql_string(&since.to_rfc3339());
    // `InferenceCall` carries no `created_at`: it is the call's own lifecycle
    // that times it. `queued_at` is written once at admission and never revised,
    // so it is the column a window can be taken over; `started_at`/`ended_at`
    // are stamped later and a queued call has neither.
    let query = format!(
        r#"{{ InferenceCall(filter: {{backend_id: {{_eq: "{backend_id}"}}, queued_at: {{_gt: "{since_text}"}}}}, order: {{queued_at: DESC}}, limit: 200) {{ call_state failure_reason queued_at provider_total_tokens }} }}"#
    );
    let response = access.execute(&query).await.ok()?;
    let rows = response["data"]["InferenceCall"].as_array()?;
    let mut calls = Vec::with_capacity(rows.len());
    let mut consumed: u64 = 0;
    let mut saw_usage = false;
    for row in rows {
        let Some(queued_at) = row["queued_at"]
            .as_str()
            .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
            .map(|at| at.with_timezone(&Utc))
        else {
            continue;
        };
        // The node's own consumption: unique calls only, and the provider's
        // total when it reported one rather than a sum of subcomponents.
        if let Some(total) = row["provider_total_tokens"].as_u64() {
            consumed = consumed.saturating_add(total);
            saw_usage = true;
        }
        calls.push(CallEvidence {
            at: queued_at,
            call_state: row["call_state"].as_str().unwrap_or_default().to_owned(),
            failure_reason: row["failure_reason"].as_str().map(str::to_owned),
        });
    }
    // The classifier's detail is already bounded (`DETAIL_MAX_CHARS`), so the
    // view takes it as it is rather than truncating a second time: the agent
    // JSON, the account inventory and the desktop show the same excerpt.
    standing_refusal(&calls, now, saw_usage.then_some(consumed), usize::MAX)
}

/// Evidence read as one Agent: the calls this backend made that this Agent's
/// own `children` grant admits.
pub async fn refusal_for_backend_as_agent(
    node: &std::sync::Arc<defra_node::Node>,
    signer: &gents_store::identity::ManagedAgentSigner,
    backend: &gents_documents::inference_backend::InferenceBackend,
    now: DateTime<Utc>,
) -> Option<UsageRefusalView> {
    let actor = identity::Did::new(signer.did()).ok()?;
    let access = gents_store::config_client::ConfigAccess::local_with_agent_signer(
        node.clone(),
        actor,
        signer.clone(),
    )
    .ok()?;
    refusal_for_backend(&access, backend, now).await
}

/// The standing refusal for one backend, across every Agent this runtime can
/// read evidence as.
///
/// `InferenceCall` is in the `children` policy resource, so the runtime key
/// alone reads none of it: each row admits the Agent that worked it. The
/// refusal is therefore read once per Agent and the newest refusal among them
/// stands — the same rule the Lean model states for one list, applied to the
/// evidence the runtime holds. With no Agent, nothing is evidenced.
pub async fn refusal_for_backend_across_agents(
    node: &std::sync::Arc<defra_node::Node>,
    signers: &[gents_store::identity::ManagedAgentSigner],
    backend: &gents_documents::inference_backend::InferenceBackend,
    now: DateTime<Utc>,
) -> Option<UsageRefusalView> {
    let mut newest: Option<UsageRefusalView> = None;
    let mut consumed: Option<u64> = None;
    for signer in signers {
        let Some(view) = refusal_for_backend_as_agent(node, signer, backend, now).await else {
            continue;
        };
        consumed = match (consumed, view.consumed_by_node) {
            (Some(total), Some(here)) => Some(total.saturating_add(here)),
            (None, here) => here,
            (total, None) => total,
        };
        let later = match &newest {
            Some(current) => view.at.as_str() > current.at.as_str(),
            None => true,
        };
        if later {
            newest = Some(view);
        }
    }
    newest.map(|view| UsageRefusalView {
        consumed_by_node: consumed,
        ..view
    })
}

/// The refusal text an operator sees: bounded, never the whole body.
fn bounded_detail(detail: &str, limit: usize) -> String {
    if detail.chars().count() <= limit {
        return detail.to_owned();
    }
    detail.chars().take(limit).collect::<String>() + "…"
}

/// `now - REFUSAL_EVIDENCE_WINDOW_HOURS`, for the caller's query.
pub fn evidence_since(now: DateTime<Utc>) -> DateTime<Utc> {
    now - Duration::hours(REFUSAL_EVIDENCE_WINDOW_HOURS)
}

/// Whether the evidence window is the provider's own or this node's analytical
/// choice. It is analytical unless the refusal names one; the view says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowProvenance {
    /// The provider stated the window in its refusal.
    ProviderReported,
    /// This node chose the window to bound a query.
    Analytical,
}

impl UsageRefusalView {
    /// Whether this view's window came from the provider or from the query.
    pub fn window_provenance(&self) -> WindowProvenance {
        match self.window {
            Some(_) => WindowProvenance::ProviderReported,
            None => WindowProvenance::Analytical,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_lean_vocab::families::provider_usage_refusal_standing as wire;

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn grok_refusal(minute: u32) -> CallEvidence {
        CallEvidence {
            at: at(&format!("2026-11-17T09:{minute:02}:00Z")),
            call_state: "failed".to_owned(),
            failure_reason: Some(
                "ProviderError: you have used all the included free usage for this account; \
                 your rolling 24-hour window will refresh later; tokens 691791/600000"
                    .to_owned(),
            ),
        }
    }

    fn completed(minute: u32) -> CallEvidence {
        CallEvidence {
            at: at(&format!("2026-11-17T09:{minute:02}:00Z")),
            call_state: "completed".to_owned(),
            failure_reason: None,
        }
    }

    /// The refusal the item exists for: evidence with no stated reset stands, and
    /// its window and counter come from the text.
    #[test]
    fn a_refusal_with_no_stated_reset_stands_with_its_stated_fields() {
        let now = at("2026-11-17T10:00:00Z");
        let view = standing_refusal(&[grok_refusal(30)], now, Some(1200), 300)
            .expect("the refusal stands");
        assert_eq!(view.window.as_deref(), Some("rolling 24-hour"));
        assert_eq!(view.used, Some(691_791));
        assert_eq!(view.cap, Some(600_000));
        assert!(view.resets_at.is_none(), "no reset was stated");
        assert_eq!(view.consumed_by_node, Some(1200));
        assert_eq!(
            view.window_provenance(),
            WindowProvenance::ProviderReported,
            "the provider named the window"
        );
    }

    /// A completion after the refusal clears it.
    #[test]
    fn a_later_completion_clears_the_refusal() {
        let now = at("2026-11-17T10:00:00Z");
        assert!(
            standing_refusal(&[grok_refusal(30), completed(45)], now, None, 300).is_none(),
            "a later success means the account is usable again"
        );
    }

    /// An earlier success does not clear a newer refusal.
    #[test]
    fn an_earlier_completion_leaves_the_refusal() {
        let now = at("2026-11-17T10:00:00Z");
        assert!(
            standing_refusal(&[completed(10), grok_refusal(30)], now, None, 300).is_some(),
            "a success before the refusal says nothing about it"
        );
    }

    /// No evidence at all is not a refusal, and a bare throttle is not one
    /// either: only a usage refusal establishes exhaustion.
    #[test]
    fn no_refusal_evidence_is_no_refusal() {
        let now = at("2026-11-17T10:00:00Z");
        assert!(standing_refusal(&[], now, None, 300).is_none());
        assert!(standing_refusal(&[completed(30)], now, None, 300).is_none());
        let throttled = CallEvidence {
            at: at("2026-11-17T09:30:00Z"),
            call_state: "failed".to_owned(),
            failure_reason: Some("ProviderError: 429 rate limit reached".to_owned()),
        };
        assert!(
            standing_refusal(&[throttled], now, None, 300).is_none(),
            "a bare rate limit is a throttle, not exhaustion"
        );
    }

    /// A refusal that stated an absolute reset stops standing once it passes.
    #[test]
    fn a_stated_reset_ends_the_refusal() {
        let past = at("2026-11-17T10:00:00Z");
        let expired = CallEvidence {
            at: at("2026-11-17T09:30:00Z"),
            call_state: "failed".to_owned(),
            failure_reason: Some("ProviderError: usage limit reached".to_owned()),
        };
        // The classifier reads the marker the same text was rendered with, so a
        // reset has to arrive in the text; this one carries none, so it stands.
        assert!(standing_refusal(std::slice::from_ref(&expired), past, None, 300).is_some());
    }

    /// The detail an operator sees is bounded, never the whole body.
    #[test]
    fn the_detail_is_bounded() {
        let long = CallEvidence {
            at: at("2026-11-17T09:30:00Z"),
            call_state: "failed".to_owned(),
            failure_reason: Some(format!(
                "ProviderError: usage limit reached {}",
                "x".repeat(600)
            )),
        };
        let view = standing_refusal(&[long], at("2026-11-17T10:00:00Z"), None, 40).expect("stands");
        assert!(view.detail.chars().count() <= 41, "{}", view.detail);
        assert!(view.detail.ends_with('…'), "{}", view.detail);
    }

    /// The Lean `provider_usage_refusal_standing` family, replayed against this
    /// implementation rather than a table copied from it. Each case's evidence
    /// is rendered the way a persisted call renders it — the classifier's own
    /// `Display` text in `failure_reason` — so the replay also proves the
    /// classifier reads back what it wrote. Whole seconds since the epoch are
    /// the family's `Int` moments: that is the resolution the classifier renders
    /// a reset at, so a reset survives the round trip exactly.
    #[test]
    fn the_refusal_rules_follow_the_executable_model() {
        use gents_loop::provider_limit::persisted_failure_reason;

        let moment = |seconds: i64| {
            DateTime::<Utc>::from_timestamp(seconds, 0).expect("a representable moment")
        };
        let moment_of = |at: DateTime<Utc>| at.timestamp();
        // The text a refused call persisted, built the way production builds it:
        // the provider's own words, the classifier's marker when it stated an
        // absolute reset, then the classifier's `Display` rendering. So a reset
        // reaches the derivation the only way it ever does — through the text.
        let rendered = |window: Option<&str>,
                        used: Option<u64>,
                        cap: Option<u64>,
                        reset: Option<i64>| {
            let now = moment(reset.unwrap_or_default());
            let statement = match (window, used, cap) {
                (Some(window), Some(used), Some(cap)) => {
                    format!("; {window} window; tokens {used}/{cap}")
                }
                _ => String::new(),
            };
            // The marker's own leading space is part of it: without it the
            // classifier reads the annotation as prose and the reset is lost.
            let marker = reset.map_or_else(String::new, |reset| {
                format!(" [provider-limit reset-at={}]", moment(reset).to_rfc3339())
            });
            // "status code 429" is how the provider's status reaches the
            // classifier; the marker itself is only read from a statused text.
            let raw = format!(
                "ProviderError: status code 429, you have used all the included free usage{statement}{marker}"
            );
            persisted_failure_reason(&raw, now)
        };

        let call = |evidence: &wire::CallWire| CallEvidence {
            at: moment(evidence.observed_at),
            call_state: if evidence.completed {
                "completed"
            } else {
                "failed"
            }
            .to_owned(),
            failure_reason: evidence.refusal.as_ref().map(|refusal| {
                rendered(
                    refusal.window.as_deref(),
                    refusal.used,
                    refusal.cap,
                    refusal.reset_at,
                )
            }),
        };
        wire::replay(|input: &wire::RefusalInput| {
            let calls: Vec<CallEvidence> = input.calls.iter().map(call).collect();
            let established = standing_refusal(&calls, moment(input.now), None, usize::MAX);
            let exhausted = established.is_some();
            wire::RefusalOutput {
                standing: established.map(|view: UsageRefusalView| wire::RefusalWire {
                    observed_at: moment_of(
                        DateTime::parse_from_rfc3339(&view.at)
                            .expect("the view states an instant")
                            .with_timezone(&Utc),
                    ),
                    window: view.window,
                    used: view.used,
                    cap: view.cap,
                    reset_at: view.resets_at.as_deref().map(|text| {
                        moment_of(
                            DateTime::parse_from_rfc3339(text)
                                .expect("an absolute reset is an instant")
                                .with_timezone(&Utc),
                        )
                    }),
                }),
                exhausted,
            }
        });
    }

    /// The window the query uses is this node's choice when the provider named
    /// none, and the view says which it is.
    #[test]
    fn an_unstated_window_is_marked_analytical() {
        let bare = CallEvidence {
            at: at("2026-11-17T09:30:00Z"),
            call_state: "failed".to_owned(),
            failure_reason: Some("ProviderError: usage limit reached".to_owned()),
        };
        let view =
            standing_refusal(&[bare], at("2026-11-17T10:00:00Z"), None, 300).expect("stands");
        assert!(view.window.is_none());
        assert_eq!(view.window_provenance(), WindowProvenance::Analytical);
        assert_eq!(
            evidence_since(at("2026-11-17T10:00:00Z")),
            at("2026-11-16T10:00:00Z"),
            "the query reaches back the analytical window"
        );
    }
}
