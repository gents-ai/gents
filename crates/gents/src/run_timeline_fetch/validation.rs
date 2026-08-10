use super::*;

pub(super) fn merge_timeline_request(
    rows: &mut Vec<TimelineRequestRow>,
    request: TimelineRequestRow,
) -> Result<()> {
    if let Some(existing) = rows.iter().find(|row| row.request_id == request.request_id) {
        if existing.doc_id != request.doc_id {
            anyhow::bail!(
                "request_id {} is ambiguous across AgentRequest documents {:?} and {:?}",
                request.request_id,
                existing.doc_id,
                request.doc_id
            );
        }
    } else {
        rows.push(request);
    }
    Ok(())
}

pub(super) fn ensure_unique_timeline_request_ids(rows: &[TimelineRequestRow]) -> Result<()> {
    let mut seen = std::collections::BTreeMap::<&str, &Option<String>>::new();
    for request in rows {
        if let Some(existing_doc_id) = seen.insert(&request.request_id, &request.doc_id) {
            anyhow::bail!(
                "request_id {} is ambiguous across AgentRequest documents {:?} and {:?}",
                request.request_id,
                existing_doc_id,
                request.doc_id
            );
        }
    }
    Ok(())
}

pub(super) fn timeline_session_ids(requests: &[TimelineRequestRow]) -> Vec<String> {
    requests
        .iter()
        .filter_map(|request| {
            request
                .session_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub(super) fn timeline_request_doc_ids(requests: &[TimelineRequestRow]) -> Result<Vec<String>> {
    let doc_ids = requests
        .iter()
        .map(|request| {
            required_lineage_value(
                "AgentRequest",
                &request.request_id,
                "_docID",
                request.doc_id.as_deref(),
            )
            .map(ToOwned::to_owned)
        })
        .collect::<Result<BTreeSet<_>>>()?;
    Ok(doc_ids.into_iter().collect())
}

pub(super) fn timeline_request_bindings(
    requests: &[TimelineRequestRow],
    root: &TimelineRequestRow,
) -> Result<BTreeMap<String, String>> {
    let mut bindings = BTreeMap::new();
    for request in requests.iter().chain(std::iter::once(root)) {
        let doc_id = required_lineage_value(
            "AgentRequest",
            &request.request_id,
            "_docID",
            request.doc_id.as_deref(),
        )?;
        match bindings.insert(doc_id.to_string(), request.request_id.clone()) {
            Some(existing) if existing != request.request_id => anyhow::bail!(
                "AgentRequest _docID {doc_id} is bound to both {existing} and {}",
                request.request_id
            ),
            _ => {}
        }
    }
    Ok(bindings)
}

pub(super) fn validate_request_scoped_rows(
    bindings: &BTreeMap<String, String>,
    messages: &[TimelineMessageRow],
    tool_calls: &[TimelineToolCallRow],
    responses: &[TimelineResponseRow],
    inference_calls: &[TimelineInferenceCallRow],
    compactions: &[TimelineCompactionRow],
) -> Result<()> {
    for row in messages {
        validate_optional_request_binding(
            bindings,
            "AgentMessage",
            row.doc_id.as_deref().unwrap_or("<unknown>"),
            row.request_id.as_deref(),
            row.request_doc_id.as_deref(),
        )?;
    }
    for row in tool_calls {
        validate_optional_request_binding(
            bindings,
            "AgentToolCall",
            row.doc_id.as_deref().unwrap_or(&row.tool_call_id),
            row.request_id.as_deref(),
            row.request_doc_id.as_deref(),
        )?;
    }
    for row in responses {
        validate_required_request_binding(
            bindings,
            "AgentResponse",
            row.doc_id.as_deref().unwrap_or(&row.request_id),
            &row.request_id,
            row.request_doc_id.as_deref(),
        )?;
    }
    for row in inference_calls {
        validate_required_request_binding(
            bindings,
            "InferenceCall",
            row.doc_id.as_deref().unwrap_or(&row.call_id),
            &row.request_id,
            row.request_doc_id.as_deref(),
        )?;
    }
    for row in compactions {
        // A fork copies compaction state so the child session can preserve its
        // prompt-reduction boundary, but it does not copy the parent requests.
        // Such imported session context is intentionally unbound; any bound
        // compaction must still resolve as an exact logical/physical pair.
        validate_optional_request_binding(
            bindings,
            "CompactionEntry",
            row.doc_id.as_deref().unwrap_or(&row.compaction_key),
            Some(&row.request_id),
            row.request_doc_id.as_deref(),
        )?;
    }
    Ok(())
}

pub(super) fn request_scoped_row_is_in_timeline(
    bindings: &BTreeMap<String, String>,
    request_id: Option<&str>,
    request_doc_id: Option<&str>,
) -> bool {
    let request_id = nonempty(request_id);
    let request_doc_id = nonempty(request_doc_id);
    match (request_id, request_doc_id) {
        (None, None) => true,
        // Previous schema generations wrote only the logical label. They are
        // not evidence for a physical join and must not poison the rest of the
        // timeline or be projected as verified provenance.
        (Some(_), None) => false,
        (request_id, Some(doc_id)) => {
            bindings.contains_key(doc_id)
                || request_id.is_some_and(|request_id| bindings.values().any(|id| id == request_id))
        }
    }
}

pub(super) fn validate_optional_request_binding(
    bindings: &BTreeMap<String, String>,
    collection: &str,
    label: &str,
    request_id: Option<&str>,
    request_doc_id: Option<&str>,
) -> Result<()> {
    match (nonempty(request_id), nonempty(request_doc_id)) {
        (None, None) => Ok(()),
        (Some(request_id), Some(request_doc_id)) => {
            validate_binding_pair(bindings, collection, label, request_id, request_doc_id)
        }
        _ => anyhow::bail!(
            "{collection} {label} has incomplete request lineage: request_id={request_id:?} request_doc_id={request_doc_id:?}"
        ),
    }
}

pub(super) fn validate_required_request_binding(
    bindings: &BTreeMap<String, String>,
    collection: &str,
    label: &str,
    request_id: &str,
    request_doc_id: Option<&str>,
) -> Result<()> {
    let request_id = required_lineage_value(collection, label, "request_id", Some(request_id))?;
    let request_doc_id =
        required_lineage_value(collection, label, "request_doc_id", request_doc_id)?;
    validate_binding_pair(bindings, collection, label, request_id, request_doc_id)
}

pub(super) fn validate_binding_pair(
    bindings: &BTreeMap<String, String>,
    collection: &str,
    label: &str,
    request_id: &str,
    request_doc_id: &str,
) -> Result<()> {
    match bindings.get(request_doc_id) {
        Some(expected) if expected == request_id => Ok(()),
        Some(expected) => anyhow::bail!(
            "{collection} {label} request_doc_id {request_doc_id} belongs to {expected}, not {request_id}"
        ),
        None => anyhow::bail!(
            "{collection} {label} points to AgentRequest {request_doc_id}, which is outside this timeline"
        ),
    }
}

pub(super) fn validate_child_tool_bridges(
    root: &TimelineRequestRow,
    requests: &[TimelineRequestRow],
    tool_calls: &[TimelineToolCallRow],
) -> Result<()> {
    let root_doc_id = required_lineage_value(
        "AgentRequest",
        &root.request_id,
        "_docID",
        root.doc_id.as_deref(),
    )?;
    for child in requests.iter().filter(|request| {
        nonempty(request.caused_by_parent_request_doc_id.as_deref()) == Some(root_doc_id)
    }) {
        let tool_doc_id = nonempty(child.caused_by_parent_tool_call_doc_id.as_deref());
        let logical_tool_id = nonempty(child.caused_by_parent_tool_call_id.as_deref());
        let (tool_doc_id, logical_tool_id) = match (tool_doc_id, logical_tool_id) {
            (None, None) => continue,
            (Some(tool_doc_id), Some(logical_tool_id)) => (tool_doc_id, logical_tool_id),
            _ => anyhow::bail!(
                "child AgentRequest {} has incomplete parent tool lineage",
                child.request_id
            ),
        };
        let tool = tool_calls
            .iter()
            .find(|tool| nonempty(tool.doc_id.as_deref()) == Some(tool_doc_id))
            .with_context(|| {
                format!(
                    "child AgentRequest {} points to missing AgentToolCall {tool_doc_id}",
                    child.request_id
                )
            })?;
        if nonempty(tool.request_doc_id.as_deref()) != Some(root_doc_id)
            || nonempty(tool.request_id.as_deref()) != Some(root.request_id.as_str())
            || tool.tool_call_id != logical_tool_id
            || nonempty(tool.child_request_id.as_deref()) != Some(child.request_id.as_str())
        {
            anyhow::bail!(
                "child AgentRequest {} has a mismatched physical AgentToolCall bridge {}",
                child.request_id,
                tool_doc_id
            );
        }
    }
    Ok(())
}

pub(super) fn required_lineage_value<'a>(
    collection: &str,
    label: &str,
    field: &str,
    value: Option<&'a str>,
) -> Result<&'a str> {
    nonempty(value).with_context(|| format!("{collection} {label} has no {field}"))
}

pub(super) fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}
