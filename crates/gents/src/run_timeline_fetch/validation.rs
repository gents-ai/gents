use super::error::{Result, RunTimelineError};
use super::*;

pub(super) fn merge_timeline_request(
    rows: &mut Vec<TimelineRequestRow>,
    request: TimelineRequestRow,
) -> Result<()> {
    if let Some(existing) = rows.iter().find(|row| row.request_id == request.request_id) {
        if existing.doc_id != request.doc_id {
            return Err(RunTimelineError::AmbiguousRequest {
                request_id: request.request_id,
                existing_doc_id: existing.doc_id.clone(),
                doc_id: request.doc_id,
            });
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
            return Err(RunTimelineError::AmbiguousRequest {
                request_id: request.request_id.clone(),
                existing_doc_id: existing_doc_id.clone(),
                doc_id: request.doc_id.clone(),
            });
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
            Some(existing) if existing != request.request_id => {
                return Err(RunTimelineError::AgentRequestDocBound {
                    doc_id: doc_id.to_owned(),
                    existing,
                    request_id: request.request_id.clone(),
                })
            }
            _ => {}
        }
    }
    Ok(bindings)
}

pub(super) fn validate_request_scoped_rows(
    bindings: &BTreeMap<String, String>,
    messages: &[TimelineMessageRow],
    tool_calls: &[TimelineToolCallRow],
    inference_calls: &[TimelineInferenceCallRow],
    compactions: &[TimelineCompactionRow],
    provider_context_reductions: &[TimelineProviderContextReductionRow],
    rendered_requests: &[TimelineRenderedRequestRow],
) -> Result<()> {
    for row in messages {
        if row.request_doc_id != row.header.request_doc_id {
            return Err(RunTimelineError::MessageRequestMismatch);
        }
        if let Some(doc_id) = row.header.request_doc_id.as_deref() {
            if !bindings.contains_key(doc_id) {
                return Err(RunTimelineError::UnresolvedMessageRequest {
                    message: row
                        .doc_id
                        .as_deref()
                        .unwrap_or(&row.header.message_key)
                        .to_owned(),
                    doc_id: doc_id.to_owned(),
                });
            }
        }
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
        validate_optional_request_binding(
            bindings,
            "CompactionEntry",
            row.doc_id.as_deref().unwrap_or(&row.compaction_key),
            row.request_id.as_deref(),
            row.request_doc_id.as_deref(),
        )?;
    }
    for row in rendered_requests {
        validate_optional_request_binding(
            bindings,
            "RenderedRequest",
            row.doc_id.as_deref().unwrap_or(&row.capture_key),
            row.request_id.as_deref(),
            row.request_doc_id.as_deref(),
        )?;
    }
    for row in provider_context_reductions {
        validate_required_request_binding(
            bindings,
            "ProviderContextReduction",
            &row.reduction_key,
            &row.request_id,
            Some(&row.request_doc_id),
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
        // Legacy logical-only labels are not evidence for a physical join.
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
        _ => Err(RunTimelineError::IncompleteRequestLineage {
            collection: collection.to_owned(),
            label: label.to_owned(),
            request_id: request_id.map(str::to_owned),
            request_doc_id: request_doc_id.map(str::to_owned),
        }),
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
        Some(expected) => Err(RunTimelineError::RequestDocBelongsElsewhere {
            collection: collection.to_owned(),
            label: label.to_owned(),
            request_doc_id: request_doc_id.to_owned(),
            expected: expected.clone(),
            request_id: request_id.to_owned(),
        }),
        None => Err(RunTimelineError::RequestDocOutsideTimeline {
            collection: collection.to_owned(),
            label: label.to_owned(),
            request_doc_id: request_doc_id.to_owned(),
        }),
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
            _ => {
                return Err(RunTimelineError::IncompleteParentToolLineage {
                    request_id: child.request_id.clone(),
                })
            }
        };
        let tool = tool_calls
            .iter()
            .find(|tool| nonempty(tool.doc_id.as_deref()) == Some(tool_doc_id))
            .ok_or_else(|| RunTimelineError::MissingBridgeTool {
                request_id: child.request_id.clone(),
                tool_doc_id: tool_doc_id.to_owned(),
            })?;
        if nonempty(tool.request_doc_id.as_deref()) != Some(root_doc_id)
            || nonempty(tool.request_id.as_deref()) != Some(root.request_id.as_str())
            || tool.tool_call_id != logical_tool_id
        {
            return Err(RunTimelineError::MismatchedBridgeTool {
                request_id: child.request_id.clone(),
                tool_doc_id: tool_doc_id.to_owned(),
            });
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
    nonempty(value).ok_or_else(|| RunTimelineError::MissingLineage {
        collection: collection.to_owned(),
        label: label.to_owned(),
        field: field.to_owned(),
    })
}

pub(super) fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}
