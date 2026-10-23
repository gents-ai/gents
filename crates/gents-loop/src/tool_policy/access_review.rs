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

    #[derive(Deserialize)]
    struct Snapshot {
        access_review_cases: Vec<Case>,
    }

    #[derive(Deserialize)]
    #[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
    enum Case {
        Context {
            required: u64,
            limit: u64,
            costs: Vec<u64>,
            expected: Option<usize>,
        },
        Reply {
            action: String,
            raw: String,
            expected: Review,
        },
        Trace {
            steps: Vec<Step>,
        },
        Completion {
            purpose: gents_protocol::request_admission::RequestPurpose,
            #[serde(rename = "hasTools")]
            has_tools: bool,
            expected: bool,
        },
        Route {
            input: Input<u64, u64>,
            history: Vec<Review>,
            verdict: Review,
            grant: Grant<u64, u64>,
            expected: Route,
            #[serde(rename = "expectedHistory")]
            expected_history: Vec<Review>,
        },
    }

    #[derive(Deserialize)]
    struct Step {
        verdict: Review,
        history: Vec<Review>,
        blocked: bool,
    }

    #[test]
    fn generated_access_review_cases_drive_native_reply_and_routing() {
        let snapshot: Snapshot = gents_lean_contract::load_contract_snapshot().unwrap();
        assert!(!snapshot.access_review_cases.is_empty());
        for case in snapshot.access_review_cases {
            match case {
                Case::Context {
                    required,
                    limit,
                    costs,
                    expected,
                } => {
                    assert_eq!(select_context(required, limit, costs), expected);
                }
                Case::Reply {
                    action,
                    raw,
                    expected,
                } => assert_eq!(decode_reply(&action, &raw), expected),
                Case::Trace { steps } => {
                    let mut history = History::default();
                    for step in steps {
                        history.record(step.verdict);
                        assert_eq!(history.verdicts, step.history);
                        assert_eq!(history.blocked(), step.blocked);
                    }
                }
                Case::Completion {
                    purpose,
                    has_tools,
                    expected,
                } => {
                    assert_eq!(accepts_completion(purpose, has_tools), expected);
                }
                Case::Route {
                    input,
                    history,
                    verdict,
                    grant,
                    expected,
                    expected_history,
                } => {
                    let mut history = History { verdicts: history };
                    assert_eq!(evaluate(&input, &history, verdict, &grant), expected);
                    assert_eq!(apply(&input, &mut history, verdict, &grant), expected);
                    assert_eq!(history.verdicts, expected_history);
                }
            }
        }
    }

    #[test]
    fn duplicate_fields_cannot_select_a_more_permissive_verdict() {
        let raw =
            r#"{"actionId":"action-1","reason":"Ambiguous","decision":"deny","decision":"allow"}"#;
        assert_eq!(decode_reply("action-1", raw), Review::Unavailable);
    }
}
