use serde::{Deserialize, Serialize};

/// Lean `GoalProgress.CallPrint`: one tool call of a goal turn by name,
/// canonical arguments and result digest; never the Agent's text.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CallPrint {
    pub tool: String,
    pub arguments: String,
    pub result: String,
}

/// Lean `GoalProgress.Record`: what the Goal owner keeps on the Goal when it
/// decides a continuation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressRecord {
    pub streak: u64,
    pub previous: Vec<CallPrint>,
}

/// Lean `GoalProgress.noProgressLimit`.
pub const NO_PROGRESS_LIMIT: u64 = 3;

/// Lean `GoalProgress.step`: the record a finished goal turn leaves and
/// whether the Goal pauses. `live_deadline` is the deadline of the Goal's
/// own running background tool call, if any.
pub fn progress_step(
    record: &ProgressRecord,
    calls: Vec<CallPrint>,
    goal_state_changed: bool,
    live_deadline: Option<u64>,
    now: u64,
) -> (ProgressRecord, bool) {
    let progressed = goal_state_changed || calls.iter().any(|call| !record.previous.contains(call));
    if progressed {
        (
            ProgressRecord {
                streak: 0,
                previous: calls,
            },
            false,
        )
    } else if live_deadline.is_some_and(|deadline| now < deadline) {
        (
            ProgressRecord {
                streak: record.streak,
                previous: calls,
            },
            false,
        )
    } else {
        let streak = record.streak + 1;
        (
            ProgressRecord {
                streak,
                previous: calls,
            },
            NO_PROGRESS_LIMIT <= streak,
        )
    }
}

/// A call's arguments in canonical JSON: object keys sorted, no
/// insignificant whitespace. Arguments that are not JSON stay as given.
pub fn canonical_arguments(arguments: &str) -> String {
    serde_json::from_str::<serde_json::Value>(arguments)
        .map(|value| canonical(&value))
        .unwrap_or_else(|_| arguments.to_owned())
}

fn canonical(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort();
            let fields: Vec<String> = keys
                .into_iter()
                .map(|key| {
                    format!(
                        "{}:{}",
                        serde_json::Value::String(key.clone()),
                        canonical(&map[key])
                    )
                })
                .collect();
            format!("{{{}}}", fields.join(","))
        }
        serde_json::Value::Array(items) => {
            let items: Vec<String> = items.iter().map(canonical).collect();
            format!("[{}]", items.join(","))
        }
        other => other.to_string(),
    }
}

/// The digest of a call's result text.
pub fn result_digest(result: &str) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(result.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
