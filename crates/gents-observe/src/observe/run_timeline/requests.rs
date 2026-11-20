use super::events::{RunTimelineEvent, TimelineRequestEvent};
use super::rows::{TimelineRequestRow, TimelineToolCallRow};
use super::values::{nonempty, timestamp_millis};

pub fn child_bridge_is_corroborated(
    root_request: &TimelineRequestRow,
    child_request: &TimelineRequestRow,
    tool_calls: &[TimelineToolCallRow],
) -> bool {
    if nonempty(child_request.caused_by_parent_request_id.as_deref())
        != Some(root_request.request_id.as_str())
    {
        return false;
    }
    let Some(root_doc_id) = nonempty(root_request.doc_id.as_deref()) else {
        return false;
    };
    if nonempty(child_request.caused_by_parent_request_doc_id.as_deref()) != Some(root_doc_id) {
        return false;
    }
    if request_only_control_link_is_corroborated(child_request) {
        return true;
    }
    let Some(parent_tool_call_id) =
        nonempty(child_request.caused_by_parent_tool_call_id.as_deref())
    else {
        return false;
    };
    let Some(parent_tool_doc_id) =
        nonempty(child_request.caused_by_parent_tool_call_doc_id.as_deref())
    else {
        return false;
    };
    tool_calls.iter().any(|tool_call| {
        nonempty(tool_call.doc_id.as_deref()) == Some(parent_tool_doc_id)
            && nonempty(tool_call.request_doc_id.as_deref()) == Some(root_doc_id)
            && nonempty(tool_call.request_id.as_deref()) == Some(root_request.request_id.as_str())
            && tool_call.tool_call_id == parent_tool_call_id
    })
}

fn request_only_control_link_is_corroborated(request: &TimelineRequestRow) -> bool {
    if nonempty(request.caused_by_parent_tool_call_id.as_deref()).is_some()
        || nonempty(request.caused_by_parent_tool_call_doc_id.as_deref()).is_some()
    {
        return false;
    }
    if gents_session::request::lifecycle::is_background_completion_request(&request.input) {
        return true;
    }
    request.input.queue.as_ref().is_some_and(|hints| {
        matches!(
            hints.source,
            gents_session::request::lifecycle::queue::QueueSource::User
                | gents_session::request::lifecycle::queue::QueueSource::Steering
                | gents_session::request::lifecycle::queue::QueueSource::Goal
        )
    })
}

pub(super) fn push_request_event(events: &mut Vec<RunTimelineEvent>, request: &TimelineRequestRow) {
    events.push(RunTimelineEvent::Request(Box::new(TimelineRequestEvent {
        request_id: request.request_id.clone(),
        parent_request_id: request.caused_by_parent_request_id.clone(),
        parent_tool_call_id: request.caused_by_parent_tool_call_id.clone(),
        caused_by_trigger_id: request.caused_by_trigger_id.clone(),
        caused_by_trigger_kind: request.caused_by_trigger_kind.clone(),
        caused_by_correlation: request.caused_by_correlation.clone(),
        caused_by_trigger_context: request.caused_by_trigger_context.clone(),
        caused_by_source_doc_id: request.caused_by_source_doc_id.clone(),
        node_did: request.node_did.clone(),
        agent_did: request.agent_did.clone(),
        session_id: request.session_id.clone(),
        lifecycle_state: request
            .lifecycle_state
            .map(|state| state.as_str().to_string()),
        failure_reason: request.failure_reason.clone(),
        content: request.content.clone(),
        input: request.input.clone(),
        execution_origin: request.execution_origin.clone(),
        timestamp: request.created_at.clone(),
        retry_summary: request.retry_summary.clone(),
    })));
}

pub(super) fn resolve_logical_request_id_for_doc<'a>(
    request_doc_id: Option<&str>,
    requests: impl Iterator<Item = &'a TimelineRequestRow>,
) -> Option<&'a str> {
    let request_doc_id = nonempty(request_doc_id)?;
    let mut matching = requests.filter(|request| request.doc_id.as_deref() == Some(request_doc_id));
    let first = matching.next()?;
    matching
        .all(|row| row.request_id == first.request_id)
        .then_some(first.request_id.as_str())
}

pub(super) fn infer_request_id_for_tool_call(
    tool_call: &TimelineToolCallRow,
    requests: &[TimelineRequestRow],
) -> Option<String> {
    if let Some(request_id) = nonempty(tool_call.request_id.as_deref()) {
        return Some(request_id.to_string());
    }

    if let Some(started_at) = tool_call.started_at.as_deref().and_then(timestamp_millis) {
        if let Some(request_id) = requests
            .iter()
            .filter(|request| request.session_id.as_deref() == Some(tool_call.session_id.as_str()))
            .filter_map(|request| {
                let created_at = request.created_at.as_deref().and_then(timestamp_millis)?;
                (created_at <= started_at).then_some((request.request_id.as_str(), created_at))
            })
            .max_by_key(|(_, created_at)| *created_at)
            .map(|(request_id, _)| request_id)
        {
            return Some(request_id.to_string());
        }
    }

    let mut session_requests = requests
        .iter()
        .filter(|request| request.session_id.as_deref() == Some(tool_call.session_id.as_str()));
    let request = session_requests.next()?;
    session_requests
        .next()
        .is_none()
        .then(|| request.request_id.clone())
}
