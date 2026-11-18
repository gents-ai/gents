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
use rapidhash::{HashSetExt, RapidHashSet};

pub use crate::usage_observation::WindowBasis as WindowProvenance;
use crate::usage_observation::{UsageRefusalView, WindowBasis};

/// How far back the evidence window reaches. An analytical choice, not a
/// provider fact: a refusal whose reset is unknown is looked for in this window
/// and the view says the window is analytical.
pub const REFUSAL_EVIDENCE_WINDOW_HOURS: i64 = 24;

/// What one call in the window says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallEvidence {
    /// The call's own id, so a call visible to several Agents is counted once.
    pub call_id: Option<String>,
    pub at: DateTime<Utc>,
    /// `"completed"`, `"failed"` or `"queued"`.
    pub call_state: String,
    /// The model the call ran on. A refusal is model-scoped: providers state
    /// "used all the included free usage for model grok-4.7", so both standing
    /// and clearing are decided per model rather than per account.
    pub model_name: Option<String>,
    /// The persisted failure text, which the classifier can read again.
    pub failure_reason: Option<String>,
    /// What the provider stated, carried whole in the persisted text.
    pub provider_total_tokens: Option<u64>,
}

impl CallEvidence {
    /// Whether this call ran in `model`'s scope. An unknown model on either side
    /// is its own scope, never a wildcard over every model.
    fn is_model(&self, model: Option<&str>) -> bool {
        self.model_name.as_deref() == model
    }
}

/// What this node counted in `scope`.
///
/// Scoped to the refused model when the refusal names one, so the number beside
/// a model-scoped refusal is about that model. When the refusal names no model
/// the count is the whole backend's, and the view says so through `consumed_scope`.
fn consumed_in_scope(calls: &[CallEvidence], scope: Option<&str>) -> Option<u64> {
    let totals = calls
        .iter()
        .filter(|call| scope.is_none_or(|model| call.is_model(Some(model))))
        .filter_map(|call| call.provider_total_tokens);
    let mut saw = false;
    let mut sum = 0u64;
    for total in totals {
        saw = true;
        sum = sum.saturating_add(total);
    }
    saw.then_some(sum)
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
    detail_limit: usize,
) -> Option<UsageRefusalView> {
    // Every model that still stands is evaluated separately, then the newest
    // standing one is returned. Taking only the newest refusal's scope would
    // hide an older refusal that still stands on another model: the newest can
    // be cleared by a same-model success while the older one is untouched.
    standing_refusals(calls, now, detail_limit)
        .into_iter()
        .max_by(|left, right| left.at.cmp(&right.at))
}

/// As [`standing_refusal`], stating whether the evidence read reached its bound.
pub fn standing_refusal_with(
    calls: &[CallEvidence],
    now: DateTime<Utc>,
    detail_limit: usize,
    partial: bool,
) -> Option<UsageRefusalView> {
    standing_refusals_with(calls, now, detail_limit, partial)
        .into_iter()
        .max_by(|left, right| left.at.cmp(&right.at))
}

/// Every model the evidence still shows as refused, newest first.
///
/// The scope is per model because a provider's refusal is: xAI says "used all
/// the included free usage for model grok-4.7". A completion on a sibling model
/// is evidence about that model, so it leaves the refusal standing here and a
/// refusal on one model does not mark the rest of the account out.
pub fn standing_refusals(
    calls: &[CallEvidence],
    now: DateTime<Utc>,
    detail_limit: usize,
) -> Vec<UsageRefusalView> {
    standing_refusals_with(calls, now, detail_limit, false)
}

/// As [`standing_refusals`], stating whether the evidence read reached its bound.
///
/// `partial` is carried onto every view, so an operator and a model both see
/// that "no older refusal" is only true of the rows that were read.
pub fn standing_refusals_with(
    calls: &[CallEvidence],
    now: DateTime<Utc>,
    detail_limit: usize,
    partial: bool,
) -> Vec<UsageRefusalView> {
    // The models the evidence names, in first-seen order. `None` is its own
    // scope, and is included when a model-less refusal exists.
    let mut scopes: Vec<Option<String>> = Vec::new();
    for call in calls {
        if !scopes
            .iter()
            .any(|scope| scope.as_deref() == call.model_name.as_deref())
        {
            scopes.push(call.model_name.clone());
        }
    }
    let mut standing: Vec<UsageRefusalView> = scopes
        .into_iter()
        .filter_map(|scope| {
            // The local count is scoped to the refused model when the refusal
            // names one, so "counted on this node" is about the same thing the
            // refusal is about rather than every model of the backend.
            let consumed = consumed_in_scope(calls, scope.as_deref());
            // The scope the count is over, said out loud.
            let consumed_scope = match &scope {
                Some(model) => format!("model:{model}"),
                None => "backend".to_owned(),
            };
            standing_refusal_for_model(calls, scope.as_deref(), now, consumed, detail_limit).map(
                |mut view| {
                    view.consumed_scope = consumed_scope;
                    view.partial = partial;
                    view
                },
            )
        })
        .collect();
    // Newest first, so a caller that takes one gets the most recent statement.
    standing.sort_by(|left, right| right.at.cmp(&left.at));
    standing
}

/// Whether the evidence establishes a standing refusal **for one model**, and as
/// which view.
///
/// Scope is the model, because a provider's refusal is model-scoped: xAI says
/// "used all the included free usage for model grok-4.7". Only a call on the same
/// model is evidence about it, so a completion on a sibling model leaves it
/// standing and a refusal on one model does not mark the rest of the account out.
pub fn standing_refusal_for_model(
    calls: &[CallEvidence],
    model: Option<&str>,
    now: DateTime<Utc>,
    consumed_by_node: Option<u64>,
    detail_limit: usize,
) -> Option<UsageRefusalView> {
    let scoped: Vec<&CallEvidence> = calls.iter().filter(|call| call.is_model(model)).collect();
    // The latest refusal on this model.
    let refusal = scoped
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
    // A later completion **on this model** clears it; on another model it does
    // not, and a completion at the refusal's own moment does not either. The
    // scope compared is the refusal's own, so it is the one the view reports.
    let cleared = scoped.iter().any(|call| {
        call.call_state == "completed" && call.at > observed_at && call.is_model(model)
    });
    if cleared {
        return None;
    }

    Some(UsageRefusalView {
        at: observed_at.to_rfc3339(),
        model: model.map(str::to_owned),
        // The provider's own words, or nothing: this node never puts a window
        // it chose into the field the provider's statement belongs in.
        window_basis: match usage.window {
            Some(_) => WindowBasis::ProviderReported,
            None => WindowBasis::Unknown,
        },
        // The enum's own label, so this is the closed set's spelling and never
        // provider text.
        window: usage.window.map(|window| window.label().to_owned()),
        used: usage.used,
        cap: usage.cap,
        resets_at: usage.resets_at.map(|at| at.to_rfc3339()),
        consumed_by_node,
        // This node's own arithmetic over its own calls: an estimate, stated as
        // one wherever it is shown, and never confused with the provider's count.
        consumed_basis: WindowBasis::LocallyEstimated,
        // Overwritten by the caller that knows the scope; the per-model builder
        // alone cannot say whether the read was partial.
        consumed_scope: match model {
            Some(model) => format!("model:{model}"),
            None => "backend".to_owned(),
        },
        partial: false,
        detail: bounded_detail(&usage.detail, detail_limit),
        // The look-back this node bounded its own evidence to, stated as
        // this node's choice rather than as the provider's window.
        evidence_window_hours: REFUSAL_EVIDENCE_WINDOW_HOURS,
    })
}

/// How many rows one evidence page asks for, and the most pages a read will take.
///
/// The bound is stated rather than silent: a busy backend can exceed one page,
/// and 200 rows used to truncate the search and the token sum without saying so.
const EVIDENCE_PAGE: usize = 200;
const EVIDENCE_MAX_PAGES: usize = 5;

/// The evidence for one backend in the window, paged to the end or to a stated
/// bound, deduped by call id.
///
/// A call can be visible to more than one Agent (the reader plus the `parent`
/// edge), so the same `call_id` arrives twice; counting it twice would inflate
/// the local sum, and only the first row for an id is kept. Returns
/// `(calls, saw_usage, consumed)`; `saw_usage` distinguishes "no totals" from
/// "totalled zero".
async fn read_evidence(
    access: &gents_store::config_client::ConfigAccess,
    backend_id: &str,
    since: &DateTime<Utc>,
) -> Option<Evidence> {
    let since_text = gents_store::graphql::escape_graphql_string(&since.to_rfc3339());
    let fetch = |page: usize| {
        let query = format!(
            r#"{{ InferenceCall(filter: {{backend_id: {{_eq: "{backend_id}"}}, queued_at: {{_gt: "{since_text}"}}}}, order: {{queued_at: DESC}}, limit: {EVIDENCE_PAGE}, offset: {}) {{ call_id call_state model_name failure_reason queued_at provider_total_tokens }} }}"#,
            page * EVIDENCE_PAGE
        );
        let access = access.clone();
        async move {
            let response = access.execute(&query).await.ok()?;
            Some(response["data"]["InferenceCall"].as_array()?.clone())
        }
    };
    page_evidence(fetch).await
}

/// What a read of one backend's evidence found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Evidence {
    pub calls: Vec<CallEvidence>,
    pub saw_usage: bool,
    pub consumed: u64,
    /// The read reached its page bound before the window ended, so an older row
    /// was not seen. Carried even when no refusal stands, so "no refusal in a
    /// truncated search" is distinguishable from "none".
    pub partial: bool,
}

/// Page a fetcher to the end of the window, or to the stated bound.
///
/// Separated from the query so the bound is testable without a node: `fetch(page)`
/// returns that page's rows, or `None` if the query failed.
async fn page_evidence<F, Fut>(fetch: F) -> Option<Evidence>
where
    F: Fn(usize) -> Fut,
    Fut: std::future::Future<Output = Option<Vec<serde_json::Value>>>,
{
    let mut seen: RapidHashSet<String> = RapidHashSet::new();
    let mut evidence = Evidence {
        // A read that ends early without a short page is partial by default.
        partial: true,
        ..Evidence::default()
    };
    for page in 0..EVIDENCE_MAX_PAGES {
        let rows = fetch(page).await?;
        let short = rows.len() < EVIDENCE_PAGE;
        for row in rows {
            // Dedupe by the call's own id: one call is one call's tokens.
            if let Some(call_id) = row["call_id"].as_str() {
                if !seen.insert(call_id.to_owned()) {
                    continue;
                }
            }
            let Some(queued_at) = row["queued_at"]
                .as_str()
                .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
                .map(|at| at.with_timezone(&Utc))
            else {
                continue;
            };
            let provider_total_tokens = row["provider_total_tokens"].as_u64();
            if let Some(total) = provider_total_tokens {
                evidence.consumed = evidence.consumed.saturating_add(total);
                evidence.saw_usage = true;
            }
            evidence.calls.push(CallEvidence {
                call_id: row["call_id"].as_str().map(str::to_owned),
                at: queued_at,
                call_state: row["call_state"].as_str().unwrap_or_default().to_owned(),
                model_name: row["model_name"].as_str().map(str::to_owned),
                failure_reason: row["failure_reason"].as_str().map(str::to_owned),
                provider_total_tokens,
            });
        }
        if short {
            evidence.partial = false;
            break;
        }
    }
    Some(evidence)
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
) -> Option<(Option<UsageRefusalView>, bool)> {
    let since = evidence_since(now);
    let backend_id = gents_store::graphql::escape_graphql_string(&backend.backend_id);
    // `InferenceCall` carries no `created_at`: it is the call's own lifecycle
    // that times it. `queued_at` is written once at admission and never revised,
    // so it is the column a window can be taken over; `started_at`/`ended_at`
    // are stamped later and a queued call has neither.
    let evidence = read_evidence(access, &backend_id, &since).await?;
    // The classifier's detail is already bounded (`DETAIL_MAX_CHARS`), so the
    // view takes it as it is rather than truncating a second time: the agent
    // JSON, the account inventory and the desktop show the same excerpt.
    let view = standing_refusal_with(&evidence.calls, now, usize::MAX, evidence.partial);
    // `partial` is returned even when no view is: "no refusal in a truncated
    // search" is not the same finding as "none".
    Some((view, evidence.partial))
}

/// Evidence read as one Agent: the calls this backend made that this Agent's
/// own `children` grant admits.
pub async fn refusal_for_backend_as_agent(
    node: &std::sync::Arc<defra_node::Node>,
    signer: &gents_store::identity::ManagedAgentSigner,
    backend: &gents_documents::inference_backend::InferenceBackend,
    now: DateTime<Utc>,
) -> Option<(Option<UsageRefusalView>, bool)> {
    let evidence = evidence_as_agent(node, signer, backend, now).await?;
    let view = standing_refusal_with(&evidence.calls, now, usize::MAX, evidence.partial);
    Some((view, evidence.partial))
}

/// The raw evidence this Agent's `children` grant admits for one backend.
async fn evidence_as_agent(
    node: &std::sync::Arc<defra_node::Node>,
    signer: &gents_store::identity::ManagedAgentSigner,
    backend: &gents_documents::inference_backend::InferenceBackend,
    now: DateTime<Utc>,
) -> Option<Evidence> {
    let actor = identity::Did::new(signer.did()).ok()?;
    let access = gents_store::config_client::ConfigAccess::local_with_agent_signer(
        node.clone(),
        actor,
        signer.clone(),
    )
    .ok()?;
    let since = evidence_since(now);
    let backend_id = gents_store::graphql::escape_graphql_string(&backend.backend_id);
    read_evidence(&access, &backend_id, &since).await
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
) -> Option<(Option<UsageRefusalView>, bool)> {
    // Read each Agent's evidence, then decide once over the union.
    //
    // The union is deduped by call id: a call visible to two Agents is one call,
    // and summing per-Agent views counted its tokens twice. Deciding over the
    // union also keeps the refusal to the model it was stated for, rather than
    // "the newest refusal any Agent can see" applied to the whole account.
    let mut union: Vec<CallEvidence> = Vec::new();
    let mut seen: RapidHashSet<String> = RapidHashSet::new();
    let mut partial = false;
    for signer in signers {
        let Some(evidence) = evidence_as_agent(node, signer, backend, now).await else {
            continue;
        };
        partial |= evidence.partial;
        for call in evidence.calls {
            let key = match &call.call_id {
                Some(id) => id.clone(),
                // Without a call id there is nothing that says two rows are the
                // same call, so they are kept apart: merging distinct calls
                // would drop one and under-count. Each row gets its own key.
                None => format!("row:{}:{}", union.len(), call.at.to_rfc3339()),
            };
            if seen.insert(key) {
                union.push(call);
            }
        }
    }
    // Partial if any Agent's read was: a page bound reached by one means the
    // union has a horizon, and saying so beats implying there is none.
    let view = standing_refusal_with(&union, now, usize::MAX, partial);
    Some((view, partial))
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

impl UsageRefusalView {
    /// Whether this view's window came from the provider or from this node.
    pub fn window_provenance(&self) -> WindowProvenance {
        self.window_basis
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_lean_vocab::families::provider_usage_refusal_standing as family;

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .unwrap()
            .with_timezone(&Utc)
    }

    /// The model the Grok refusal is about: xAI states it ("for model grok-4.7"),
    /// so every refusal fixture names one.
    const GROK: &str = "grok-4.7";

    /// xAI's refusal, **captured verbatim from the live node** on 2026-11-16
    /// (2026-11-16T19:01:05Z, tool call `plugin-effect:bae-25641432…:7`).
    ///
    /// Not a reconstruction: it is 339 bytes, carries an em dash, and writes its
    /// counter as `tokens (actual/limit): 691791/600000` — the parenthesised
    /// label contains a `/` of its own, which is what a naive pair reader trips
    /// on. It also names the model and says "rolling 24-hour window".
    /// Segment by segment, so no `\` line-continuation can drop a space: the
    /// captured text is 336 characters and every space in it matters.
    const GROK_REFUSAL_CAPTURED: &str =     concat!(
        "agent stream failed: CompletionError: ProviderError: provider usage limit reached ",
        "(reset time not reported): You've used all the included free usage for model grok-4.7 ",
        "for now. Usage resets over a rolling 24-hour window — tokens (actual/limit): ",
        "691791/600000. Upgrade to a Grok subscription for higher limits: https://grok.com/supergrok",
    );

    fn grok_refusal(minute: u32) -> CallEvidence {
        CallEvidence {
            call_id: Some(format!("call-{minute}")),
            at: at(&format!("2026-11-17T09:{minute:02}:00Z")),
            call_state: "failed".to_owned(),
            model_name: Some(GROK.to_owned()),
            // Captured verbatim from the live node, 2026-11-16T19:01:05Z
            // (tool call plugin-effect:bae-25641432…:7).
            failure_reason: Some(GROK_REFUSAL_CAPTURED.to_owned()),
            provider_total_tokens: None,
        }
    }

    fn completed(minute: u32) -> CallEvidence {
        CallEvidence {
            call_id: Some(format!("done-{minute}")),
            at: at(&format!("2026-11-17T09:{minute:02}:00Z")),
            call_state: "completed".to_owned(),
            model_name: Some(GROK.to_owned()),
            failure_reason: None,
            provider_total_tokens: None,
        }
    }

    /// A call on another model of the same backend.
    fn completed_elsewhere(minute: u32) -> CallEvidence {
        CallEvidence {
            call_id: Some(format!("other-{minute}")),
            at: at(&format!("2026-11-17T09:{minute:02}:00Z")),
            call_state: "completed".to_owned(),
            model_name: Some("grok-4.7-mini".to_owned()),
            failure_reason: None,
            provider_total_tokens: None,
        }
    }

    /// The refusal the item exists for: evidence with no stated reset stands, and
    /// its window and counter come from the text.
    #[test]
    fn a_refusal_with_no_stated_reset_stands_with_its_stated_fields() {
        let now = at("2026-11-17T10:00:00Z");
        let view = standing_refusal(&[grok_refusal(30)], now, 300).expect("the refusal stands");
        assert_eq!(view.window.as_deref(), Some("rolling 24-hour"));
        assert_eq!(view.used, Some(691_791));
        assert_eq!(view.cap, Some(600_000));
        assert!(view.resets_at.is_none(), "no reset was stated");
        // The local count is derived per scope; the fixture's call reports no
        // provider total, so there is nothing to count and nothing to invent.
        assert_eq!(view.consumed_by_node, None);
        assert_eq!(view.consumed_scope, format!("model:{GROK}"));
        assert_eq!(
            view.window_provenance(),
            WindowProvenance::ProviderReported,
            "the provider named the window"
        );
    }

    /// **Item 1.** A refusal is model-scoped: a completion on a sibling model of
    /// the same backend leaves it standing, where an account-scoped read would
    /// clear it on any other model's success.
    #[test]
    fn a_completion_on_another_model_does_not_clear_the_refusal() {
        let now = at("2026-11-17T10:00:00Z");
        let evidence = [grok_refusal(30), completed_elsewhere(40)];
        let view = standing_refusal(&evidence, now, 300)
            .expect("a sibling model's success is not this model's success");
        assert_eq!(view.model.as_deref(), Some(GROK));
        assert_eq!(view.used, Some(691_791));

        // The same model's completion still clears it.
        let same = [grok_refusal(30), completed(40)];
        assert!(standing_refusal(&same, now, 300).is_none());
    }

    /// **Item 3.** The newest refusal being cleared must not hide an older one
    /// still standing on another model. Deciding per model and then reporting
    /// the newest standing one is what makes that true; deciding from the newest
    /// refusal alone would report nothing.
    #[test]
    fn a_cleared_newer_refusal_does_not_hide_an_older_standing_one() {
        let now = at("2026-11-17T10:00:00Z");
        // minthis refuses at :20 and is untouched; grok-4.7 refuses at :30 and
        // is cleared by its own success at :45.
        let mut sibling = grok_refusal(20);
        sibling.model_name = Some("grok-4.7-mini".to_owned());
        sibling.call_id = Some("sibling-refusal".to_owned());
        let evidence = [sibling, grok_refusal(30), completed(45)];

        let view = standing_refusal(&evidence, now, 300)
            .expect("the older refusal on the sibling model still stands");
        assert_eq!(
            view.model.as_deref(),
            Some("grok-4.7-mini"),
            "the standing model is the sibling, not the cleared grok-4.7"
        );

        // And the per-model listing names exactly that one.
        let all = standing_refusals(&evidence, now, 300);
        assert_eq!(all.len(), 1, "exactly one model is still refused: {all:?}");
        assert_eq!(all[0].model.as_deref(), Some("grok-4.7-mini"));
    }

    /// A refusal on one model marks only that model out: the sibling's own
    /// evidence establishes nothing, so the account is not wholly out.
    #[test]
    fn a_refusal_marks_only_the_model_it_names() {
        let now = at("2026-11-17T10:00:00Z");
        let evidence = [grok_refusal(30)];
        assert!(standing_refusal_for_model(&evidence, Some(GROK), now, None, 300).is_some());
        assert!(
            standing_refusal_for_model(&evidence, Some("grok-4.7-mini"), now, None, 300).is_none(),
            "a refusal about one model says nothing about another"
        );
    }

    /// **Item 4.** A call visible to two Agents is one call: the local count is
    /// reported once and labelled as the node's own estimate.
    #[test]
    fn the_local_count_is_labelled_as_the_nodes_own() {
        let now = at("2026-11-17T10:00:00Z");
        let mut twice = grok_refusal(30);
        twice.provider_total_tokens = Some(120);
        let view = standing_refusal(&[twice], now, 300).expect("the refusal stands");
        assert_eq!(view.consumed_by_node, Some(120));
        assert_eq!(view.consumed_basis, WindowBasis::LocallyEstimated);
        assert_eq!(view.for_model()["consumed_basis"], "locally_estimated");
    }

    /// **Item 3.** The paging bound is testable because the loop takes a
    /// fetcher: `EVIDENCE_MAX_PAGES` full pages leave the read partial, and a
    /// short page clears it.
    #[tokio::test]
    async fn a_full_page_bound_reports_partial_and_a_short_page_does_not() {
        // Ids are unique per page: a repeated id is one call, and dedupe would
        // (correctly) collapse the repeats instead of counting them.
        let page_rows = |page: usize| {
            (0..super::EVIDENCE_PAGE)
                .map(move |n| {
                    serde_json::json!({
                        "call_id": format!("call-{page}-{n}"),
                        "queued_at": "2026-11-17T09:00:00Z",
                        "call_state": "completed",
                        "model_name": GROK,
                    })
                })
                .collect::<Vec<_>>()
        };
        // Every page is full: the loop stops at the bound and says so.
        let full = super::page_evidence(|page| async move { Some(page_rows(page)) })
            .await
            .expect("the read succeeds");
        assert!(
            full.partial,
            "the bound was reached, so an older row was unseen"
        );
        assert_eq!(
            full.calls.len(),
            super::EVIDENCE_PAGE * super::EVIDENCE_MAX_PAGES,
            "the bound is the ceiling, not a longer read"
        );

        // One short page ends the window before the bound.
        let short = super::page_evidence(|page| async move {
            Some(if page == 0 {
                page_rows(0).into_iter().take(3).collect()
            } else {
                Vec::new()
            })
        })
        .await
        .expect("the read succeeds");
        assert!(!short.partial, "a short page means the window ended");
        assert_eq!(short.calls.len(), 3);
    }

    /// **Item 1.** `partial` survives when no refusal is found: the finding from
    /// a truncated search is not the finding from a complete one.
    #[tokio::test]
    async fn a_truncated_search_without_a_refusal_still_reports_partial() {
        // A full page of plain completions: no refusal, and the bound reached.
        let full = super::page_evidence(|page| async move {
            Some(
                (0..super::EVIDENCE_PAGE)
                    .map(|n| {
                        serde_json::json!({
                            "call_id": format!("call-{page}-{n}"),
                            "queued_at": "2026-11-17T09:00:00Z",
                            "call_state": "completed",
                            "model_name": GROK,
                        })
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .await
        .expect("the read succeeds");
        assert!(full.partial);
        let view =
            standing_refusal_with(&full.calls, at("2026-11-17T10:00:00Z"), 300, full.partial);
        assert!(view.is_none(), "no refusal among the rows read");
        assert!(
            full.partial,
            "yet the search was truncated, which the caller must be able to say"
        );
    }

    /// A completion after the refusal clears it.
    #[test]
    fn a_later_completion_clears_the_refusal() {
        let now = at("2026-11-17T10:00:00Z");
        assert!(
            standing_refusal(&[grok_refusal(30), completed(45)], now, 300).is_none(),
            "a later success means the account is usable again"
        );
    }

    /// An earlier success does not clear a newer refusal.
    #[test]
    fn an_earlier_completion_leaves_the_refusal() {
        let now = at("2026-11-17T10:00:00Z");
        assert!(
            standing_refusal(&[completed(10), grok_refusal(30)], now, 300).is_some(),
            "a success before the refusal says nothing about it"
        );
    }

    /// No evidence at all is not a refusal, and a bare throttle is not one
    /// either: only a usage refusal establishes exhaustion.
    #[test]
    fn no_refusal_evidence_is_no_refusal() {
        let now = at("2026-11-17T10:00:00Z");
        assert!(standing_refusal(&[], now, 300).is_none());
        assert!(standing_refusal(&[completed(30)], now, 300).is_none());
        let throttled = CallEvidence {
            call_id: Some("throttled".to_owned()),
            at: at("2026-11-17T09:30:00Z"),
            call_state: "failed".to_owned(),
            model_name: Some(GROK.to_owned()),
            failure_reason: Some("ProviderError: 429 rate limit reached".to_owned()),
            provider_total_tokens: None,
        };
        assert!(
            standing_refusal(&[throttled], now, 300).is_none(),
            "a bare rate limit is a throttle, not exhaustion"
        );
    }

    /// A refusal that stated an absolute reset stops standing once it passes.
    #[test]
    fn a_stated_reset_ends_the_refusal() {
        let past = at("2026-11-17T10:00:00Z");
        let expired = CallEvidence {
            call_id: Some("expired".to_owned()),
            at: at("2026-11-17T09:30:00Z"),
            call_state: "failed".to_owned(),
            model_name: Some(GROK.to_owned()),
            failure_reason: Some("ProviderError: usage limit reached".to_owned()),
            provider_total_tokens: None,
        };
        // The classifier reads the marker the same text was rendered with, so a
        // reset has to arrive in the text; this one carries none, so it stands.
        assert!(standing_refusal(std::slice::from_ref(&expired), past, 300).is_some());
    }

    /// The detail an operator sees is bounded, never the whole body.
    #[test]
    fn the_detail_is_bounded() {
        let long = CallEvidence {
            call_id: Some("long".to_owned()),
            at: at("2026-11-17T09:30:00Z"),
            call_state: "failed".to_owned(),
            model_name: Some(GROK.to_owned()),
            failure_reason: Some(format!(
                "ProviderError: usage limit reached {}",
                "x".repeat(600)
            )),
            provider_total_tokens: None,
        };
        let view = standing_refusal(&[long], at("2026-11-17T10:00:00Z"), 40).expect("stands");
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

        let call = |evidence: &family::CallWire| CallEvidence {
            call_id: None,
            at: moment(evidence.observed_at),
            call_state: if evidence.completed {
                "completed"
            } else {
                "failed"
            }
            .to_owned(),
            // The scope the family states on the call, so the replay exercises
            // model scoping rather than assuming one model.
            model_name: evidence.model.clone(),
            provider_total_tokens: None,
            failure_reason: evidence.refusal.as_ref().map(|refusal| {
                rendered(
                    refusal.window.as_deref(),
                    refusal.used,
                    refusal.cap,
                    refusal.reset_at,
                )
            }),
        };
        family::replay(|input: &family::RefusalInput| {
            let calls: Vec<CallEvidence> = input.calls.iter().map(call).collect();
            let established = standing_refusal(&calls, moment(input.now), usize::MAX);
            let exhausted = established.is_some();
            family::RefusalOutput {
                standing: established.map(|view: UsageRefusalView| family::RefusalWire {
                    model: view.model,
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

    /// What an Agent sees is the parsed fields and nothing quoted from the
    /// provider's body: no excerpt, and no node-internal reading length. This is
    /// the path `provider_limits` and `config accounts` hand a model.
    #[test]
    fn the_agent_facing_read_carries_no_excerpt_text() {
        let now = at("2026-11-17T10:00:00Z");
        let view = standing_refusal(&[grok_refusal(30)], now, 300).expect("the refusal stands");
        // The operator's copy does carry the excerpt.
        assert!(view.detail.contains("free usage"));

        let seen = view.for_model();
        let object = seen.as_object().expect("an object");
        assert!(
            !object.contains_key("detail"),
            "a model never sees the provider's words: {seen}"
        );
        assert!(
            !object.contains_key("evidence_window_hours"),
            "the node's look-back length is an operator reading: {seen}"
        );
        // The parsed fields are all there, and the excerpt's text appears nowhere.
        assert_eq!(object["window"], "rolling 24-hour");
        assert_eq!(object["window_basis"], "provider_reported");
        assert_eq!(object["used"], 691_791);
        assert_eq!(object["cap"], 600_000);
        assert!(object["resets_at"].is_null());
        let rendered = seen.to_string();
        assert!(
            !rendered.contains("free usage") && !rendered.contains("tokens"),
            "no fragment of the provider's body reaches a model: {rendered}"
        );
    }

    /// A window this node chose is reported as the node's, so no reader can
    /// mistake a local bound for the provider's own statement.
    #[test]
    fn a_window_this_node_chose_is_labelled_locally_estimated() {
        let view = UsageRefusalView {
            at: "2026-11-17T10:00:00Z".into(),
            model: Some(GROK.into()),
            window: Some("24h".into()),
            used: Some(10),
            cap: Some(20),
            resets_at: None,
            consumed_by_node: Some(5),
            consumed_basis: WindowBasis::LocallyEstimated,
            consumed_scope: "model:grok-4.7".into(),
            partial: false,
            detail: "quota".into(),
            window_basis: WindowBasis::LocallyEstimated,
            evidence_window_hours: 24,
        };
        assert_eq!(view.window_provenance(), WindowProvenance::LocallyEstimated);
        // and a model still sees the label, so it can tell whose window it is
        assert_eq!(view.for_model()["window_basis"], "locally_estimated");
    }

    /// A provider that states a window is reported under the provider's name —
    /// "rolling 24-hour" in xAI's own words is provider-reported, not a guess.
    #[test]
    fn a_stated_window_is_provider_reported() {
        let now = at("2026-11-17T10:00:00Z");
        let view = standing_refusal(&[grok_refusal(30)], now, 300).expect("it stands");
        assert_eq!(view.window.as_deref(), Some("rolling 24-hour"));
        assert_eq!(view.window_provenance(), WindowProvenance::ProviderReported);
    }

    /// A provider that names no window leaves the view with none, and the view
    /// says the window is unknown rather than presenting this node's look-back
    /// as the provider's window.
    #[test]
    fn an_unstated_window_is_reported_as_unknown() {
        let bare = CallEvidence {
            call_id: Some("bare".to_owned()),
            at: at("2026-11-17T09:30:00Z"),
            call_state: "failed".to_owned(),
            model_name: Some(GROK.to_owned()),
            failure_reason: Some("ProviderError: usage limit reached".to_owned()),
            provider_total_tokens: None,
        };
        let view = standing_refusal(&[bare], at("2026-11-17T10:00:00Z"), 300).expect("stands");
        assert!(
            view.window.is_none(),
            "nothing stands in for a stated window"
        );
        assert_eq!(view.window_provenance(), WindowProvenance::Unknown);
        assert_eq!(
            evidence_since(at("2026-11-17T10:00:00Z")),
            at("2026-11-16T10:00:00Z"),
            "the query reaches back this node's own look-back"
        );
    }
}
