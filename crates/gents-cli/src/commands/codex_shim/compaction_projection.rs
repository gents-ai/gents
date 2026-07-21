use codex_app_server_protocol as codex;
use serde_json::Value;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CompactionProjectionEvent {
    Started,
    Completed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct GentsCompactionProgress {
    pub(super) call_id: String,
    pub(super) call_state: String,
}

pub(super) fn decode_gents_compaction_progress(row: &Value) -> Option<GentsCompactionProgress> {
    (row.get("call_kind")?.as_str()? == "compaction").then_some(())?;
    Some(GentsCompactionProgress {
        call_id: row.get("call_id")?.as_str()?.to_string(),
        call_state: row.get("call_state")?.as_str()?.to_string(),
    })
}

/// Project the persisted admission state without inventing a parallel
/// compaction lifecycle. A terminal row can be the first replicated
/// observation, in which case Codex still needs a well-formed start/completion
/// pair. Failed and cancelled rows never claim that context was compacted. The
/// pinned Codex protocol has no failed-item notification, so clients rendering
/// a Started item must clear it when the enclosing turn terminates.
pub(super) fn compaction_projection_events(
    previous: Option<&str>,
    current: &str,
) -> Vec<CompactionProjectionEvent> {
    let previous = previous.map(str::trim);
    let current = current.trim();
    match (previous, current) {
        (None, "queued" | "running") => vec![CompactionProjectionEvent::Started],
        (None, "completed") => vec![
            CompactionProjectionEvent::Started,
            CompactionProjectionEvent::Completed,
        ],
        (Some("queued" | "running"), "completed") => {
            vec![CompactionProjectionEvent::Completed]
        }
        _ => Vec::new(),
    }
}

pub(super) fn context_compaction_item(call_id: &str) -> codex::ThreadItem {
    codex::ThreadItem::ContextCompaction {
        id: call_id.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lean_fenced_compaction_states_project_native_codex_events() {
        assert_eq!(
            compaction_projection_events(None, "running"),
            [CompactionProjectionEvent::Started]
        );
        assert_eq!(
            compaction_projection_events(None, "completed"),
            [
                CompactionProjectionEvent::Started,
                CompactionProjectionEvent::Completed
            ]
        );
        assert_eq!(
            compaction_projection_events(Some("running"), "completed"),
            [CompactionProjectionEvent::Completed]
        );
        assert!(compaction_projection_events(None, "failed").is_empty());
        assert!(compaction_projection_events(None, "cancelled").is_empty());
        assert!(compaction_projection_events(Some("running"), "failed").is_empty());
        assert!(compaction_projection_events(Some("running"), "cancelled").is_empty());
    }

    #[test]
    fn context_compaction_item_round_trips_through_pinned_protocol() {
        let item = context_compaction_item("compaction-call-1");
        let value = serde_json::to_value(&item).expect("serialize context compaction");
        serde_json::from_value::<codex::ThreadItem>(value.clone())
            .expect("context compaction must be valid for pinned Codex protocol");
        assert_eq!(value["type"], "contextCompaction");
        assert_eq!(value["id"], "compaction-call-1");
    }
}
