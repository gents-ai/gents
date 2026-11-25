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
    /// The Goal's objective and budget when the record was written; a turn
    /// that changed them changed goal state.
    #[serde(default)]
    pub goal: String,
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
                goal: record.goal.clone(),
            },
            false,
        )
    } else if live_deadline.is_some_and(|deadline| now < deadline) {
        (
            ProgressRecord {
                streak: record.streak,
                previous: calls,
                goal: record.goal.clone(),
            },
            false,
        )
    } else {
        let streak = record.streak + 1;
        (
            ProgressRecord {
                streak,
                previous: calls,
                goal: record.goal.clone(),
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

/// Lean `GoalProgress.isVolatileResultField`: whether a JSON key in a tool
/// result varies only with the read-time clock and is omitted from the
/// no-progress digest.
pub fn is_volatile_result_field(key: &str) -> bool {
    matches!(
        key,
        "read_at"
            | "elapsed_seconds"
            | "active_time_seconds"
            | "next_pending_retry_in_ms"
            | "lease_remaining_days"
            | "duration_ms"
    )
}

/// A call's result normalized for no-progress comparison: JSON objects have
/// keys sorted and volatile read-time fields omitted; non-JSON text stays as
/// given.
pub fn canonical_result(result: &str) -> String {
    serde_json::from_str::<serde_json::Value>(result)
        .map(|value| canonical_filtered(&value, true))
        .unwrap_or_else(|_| result.to_owned())
}

fn canonical(value: &serde_json::Value) -> String {
    canonical_filtered(value, false)
}

fn canonical_filtered(value: &serde_json::Value, drop_volatile: bool) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<_> = map
                .keys()
                .filter(|key| !drop_volatile || !is_volatile_result_field(key))
                .collect();
            keys.sort();
            let fields: Vec<String> = keys
                .into_iter()
                .map(|key| {
                    format!(
                        "{}:{}",
                        serde_json::Value::String(key.clone()),
                        canonical_filtered(&map[key], drop_volatile)
                    )
                })
                .collect();
            format!("{{{}}}", fields.join(","))
        }
        serde_json::Value::Array(items) => {
            let items: Vec<String> = items
                .iter()
                .map(|item| canonical_filtered(item, drop_volatile))
                .collect();
            format!("[{}]", items.join(","))
        }
        other => other.to_string(),
    }
}

/// The digest of a call's normalized result text.
pub fn result_digest(result: &str) -> String {
    use sha2::Digest;
    let normalized = canonical_result(result);
    sha2::Sha256::digest(normalized.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Whether the turn a record describes reported the Goal blocked: under
/// `confirm_once` that turn was the report's confirmation turn's predecessor,
/// so a report in the next turn pauses the Goal.
pub fn reported_blocked(record: &ProgressRecord) -> bool {
    record
        .previous
        .iter()
        .any(|call| call.tool == "update_goal" && call.arguments.contains(r#""status":"blocked""#))
}

/// What the harness observed of one finished goal turn (Lean
/// `GoalProgress.Turn`, plus the deadline of a live background job).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnObservation {
    pub calls: Vec<CallPrint>,
    pub goal_state_changed: bool,
    pub live_deadline: Option<u64>,
}

fn stream_bytes(
    segments: &[gents_protocol::output::OutputSegment],
) -> Vec<(
    gents_protocol::output::OutputSource,
    Option<gents_protocol::output::StreamPayload>,
    String,
)> {
    let mut ordered: Vec<_> = segments
        .iter()
        .filter(|segment| !segment.runs.is_empty())
        .collect();
    ordered.sort_by_key(|segment| segment.ordinal);
    let mut streams: Vec<(
        gents_protocol::output::OutputSource,
        u32,
        Option<gents_protocol::output::StreamPayload>,
        String,
    )> = Vec::new();
    for segment in ordered {
        let mut cursor = 0usize;
        for run in &segment.runs {
            let end = (cursor + run.bytes as usize).min(segment.payload.len());
            let text = segment.payload.get(cursor..end).unwrap_or_default();
            cursor = end;
            match streams
                .iter_mut()
                .find(|(source, stream, ..)| *source == segment.source && *stream == run.stream)
            {
                Some((_, _, declaration, bytes)) => {
                    if declaration.is_none() {
                        *declaration = run.declaration.as_ref().map(|d| d.payload.clone());
                    }
                    bytes.push_str(text);
                }
                None => streams.push((
                    segment.source.clone(),
                    run.stream,
                    run.declaration.as_ref().map(|d| d.payload.clone()),
                    text.to_owned(),
                )),
            }
        }
    }
    streams
        .into_iter()
        .map(|(source, _, declaration, bytes)| (source, declaration, bytes))
        .collect()
}

/// The turn's call prints from the latest goal request's `AgentToolCall`
/// rows and its `AgentOutputSegment` rows: each call's name, its arguments
/// as the provider turn emitted them (canonicalized) and a digest of its
/// output stream's bytes. The row's `terminal_output` is not part of it: it
/// names the call's own closing record, so it differs on every call. A running call whose deadline is after `now` (seconds) is live
/// background work.
pub fn observe_turn(
    tool_calls: &[serde_json::Value],
    segments: &[gents_protocol::output::OutputSegment],
    now: u64,
) -> TurnObservation {
    use gents_protocol::output::{OutputSource, StreamPayload};
    let streams = stream_bytes(segments);
    let arguments = |call_id: &str| {
        streams
            .iter()
            .find_map(|(source, declaration, bytes)| match (source, declaration) {
                (
                    OutputSource::ProviderTurn { .. },
                    Some(StreamPayload::ToolArguments {
                        id,
                        call_id: native,
                        ..
                    }),
                ) if id == call_id || native.as_deref() == Some(call_id) => Some(bytes.as_str()),
                _ => None,
            })
    };
    let output = |doc_id: &str| {
        streams
            .iter()
            .filter(|(source, ..)| {
                matches!(source, OutputSource::ToolCall { tool_call_doc_id } if tool_call_doc_id == doc_id)
            })
            .map(|(_, _, bytes)| bytes.as_str())
            .collect::<String>()
    };
    let mut observation = TurnObservation::default();
    for row in tool_calls {
        let tool = row["tool_name"].as_str().unwrap_or_default().to_owned();
        let state = row["lifecycle_state"].as_str().unwrap_or_default();
        let doc_id = row["_docID"].as_str().unwrap_or_default();
        let call_id = row["tool_call_id"].as_str().unwrap_or_default();
        if state == "running" {
            let deadline = row["deadline_at"]
                .as_str()
                .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
                .and_then(|at| u64::try_from(at.timestamp()).ok());
            if let Some(deadline) = deadline.filter(|deadline| now < *deadline) {
                observation.live_deadline = Some(
                    observation
                        .live_deadline
                        .map_or(deadline, |d| d.max(deadline)),
                );
            }
        }
        observation.calls.push(CallPrint {
            tool,
            arguments: canonical_arguments(arguments(call_id).unwrap_or_default()),
            result: result_digest(&output(doc_id)),
        });
    }
    observation
}
