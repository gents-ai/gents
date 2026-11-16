use serde::Deserialize;

use super::workspace_permissions::{self as policy, Grant, Input, Review, Route};

pub(crate) fn accepts_completion(
    purpose: gents_protocol::request_admission::RequestPurpose,
    has_tools: bool,
) -> bool {
    !has_tools || !purpose.is_auxiliary()
}

/// Costs are exact incremental wire bytes in newest-first order, including
/// escaping and separators. The finite input retains only whole recent entries.
pub fn select_context(
    required: u64,
    limit: u64,
    costs: impl IntoIterator<Item = u64>,
) -> Option<usize> {
    let mut remaining = limit.checked_sub(required)?;
    let mut count = 0;
    for cost in costs {
        let Some(next) = remaining.checked_sub(cost) else {
            break;
        };
        remaining = next;
        count += 1;
    }
    Some(count)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Reply {
    action_id: String,
    decision: Verdict,
    reason: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum Verdict {
    Allow,
    Ask,
    Deny,
}

pub fn decode_reply(action: &str, raw: &str) -> Review {
    if raw.len() > 8192 || action.is_empty() {
        return Review::Unavailable;
    }
    let Ok(reply) = serde_json::from_str::<Reply>(raw) else {
        return Review::Unavailable;
    };
    if reply.action_id != action || reply.reason.is_empty() {
        return Review::Unavailable;
    }
    match reply.decision {
        Verdict::Allow => Review::Allow,
        Verdict::Ask => Review::Ask,
        Verdict::Deny => Review::Deny,
    }
}

#[derive(Debug, Default)]
pub struct History {
    verdicts: Vec<Review>,
}

impl History {
    fn record(&mut self, verdict: Review) {
        if matches!(verdict, Review::Allow | Review::Deny) {
            self.verdicts.insert(0, verdict);
            self.verdicts.truncate(50);
        }
    }

    pub fn blocked(&self) -> bool {
        self.verdicts
            .iter()
            .take_while(|&&verdict| verdict == Review::Deny)
            .count()
            >= 3
            || self
                .verdicts
                .iter()
                .filter(|&&verdict| verdict == Review::Deny)
                .count()
                >= 10
    }
}

pub fn evaluate<W, A>(
    input: &Input<W, A>,
    history: &History,
    verdict: Review,
    grant: &Grant<W, A>,
) -> Route
where
    W: PartialEq,
    A: PartialEq,
{
    if policy::route(input) == Route::Review && history.blocked() {
        Route::Ask
    } else {
        policy::reviewed_route(input, verdict, grant)
    }
}

pub fn apply<W, A>(
    input: &Input<W, A>,
    history: &mut History,
    verdict: Review,
    grant: &Grant<W, A>,
) -> Route
where
    W: PartialEq,
    A: PartialEq,
{
    let result = evaluate(input, history, verdict, grant);
    if policy::route(input) == Route::Review
        && !history.blocked()
        && grant.scope == input.current
        && grant.action == input.action
    {
        history.record(verdict);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    use gents_lean_vocab::families::{
        access_review_denial_trace as trace, access_review_review_route as route,
    };
    use policy::Scope;

    fn wire<T>(value: &T) -> String
    where
        T: serde::Serialize,
    {
        serde_json::to_value(value)
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn parsed<T>(wire: &str) -> T
    where
        T: serde::de::DeserializeOwned,
    {
        serde_json::from_value(serde_json::Value::String(wire.to_owned())).unwrap()
    }

    fn grant(wire: &route::WireGrant) -> Grant<u64, u64> {
        Grant {
            scope: Scope {
                workspace: wire.scope.workspace,
                revision: wire.scope.revision,
            },
            action: wire.action,
        }
    }

    #[test]
    fn generated_access_review_replies_match_the_native_decoder() {
        gents_lean_vocab::families::access_review_reply_decision::replay(|i| {
            wire(&decode_reply(&i.action, &i.raw))
        });
    }

    #[test]
    fn generated_access_review_traces_match_the_native_history() {
        gents_lean_vocab::families::access_review_denial_trace::replay(|i| {
            let mut history = History::default();
            i.verdicts
                .iter()
                .map(|verdict| {
                    history.record(parsed(verdict));
                    trace::TraceStep {
                        history: history.verdicts.iter().map(wire).collect(),
                        blocked: history.blocked(),
                    }
                })
                .collect()
        });
    }

    #[test]
    fn generated_access_review_routes_match_the_native_evaluation() {
        gents_lean_vocab::families::access_review_review_route::replay(|i| {
            let input = Input {
                current: Scope {
                    workspace: i.input.current.workspace,
                    revision: i.input.current.revision,
                },
                configured_workspace: i.input.configured_workspace,
                mode: parsed(&i.input.mode),
                action: i.input.action,
                ceiling_allows: i.input.ceiling_allows,
                crosses_boundary: i.input.crosses_boundary,
                grant: i.input.grant.as_ref().map(grant),
            };
            let mut history = History {
                verdicts: i.history.iter().map(|verdict| parsed(verdict)).collect(),
            };
            let verdict: Review = parsed(&i.verdict);
            let reviewed = grant(&i.grant);
            let evaluated = evaluate(&input, &history, verdict, &reviewed);
            let applied = apply(&input, &mut history, verdict, &reviewed);
            route::RouteOutput {
                evaluated: wire(&evaluated),
                applied: wire(&applied),
                history: history.verdicts.iter().map(wire).collect(),
            }
        });
    }

    #[test]
    fn generated_access_review_completions_match_the_native_gate() {
        gents_lean_vocab::families::access_review_completion_acceptance::replay(|i| {
            accepts_completion(parsed(&i.purpose), i.has_tools)
        });
    }

    #[test]
    fn generated_access_review_contexts_match_the_native_selection() {
        gents_lean_vocab::families::access_review_context_selection::replay(|i| {
            select_context(i.required, i.limit, i.costs.iter().copied()).map(|count| count as u64)
        });
    }

    #[test]
    fn duplicate_fields_cannot_select_a_more_permissive_verdict() {
        let raw =
            r#"{"actionId":"action-1","reason":"Ambiguous","decision":"deny","decision":"allow"}"#;
        assert_eq!(decode_reply("action-1", raw), Review::Unavailable);
    }
}
