use super::{
    atif, AdapterProjection, AdapterProjectionContractError, AdapterProjectionEnvelope,
    LangGraphStateHistoryProjection, MultiAgentTaskProjection, OpenAiCodexRunTraceProjection,
    OpenAiCodexTraceItem, ProjectionSourceVersionStatus, ADAPTER_PROJECTION_VERSION,
    EXTERNAL_ADAPTER_CAPTURE_PROJECTION_ID, RUN_TIMELINE_PROJECTION_ID,
};

pub fn validate_adapter_projection_contract(
    envelope: &AdapterProjectionEnvelope,
) -> Result<(), AdapterProjectionContractError> {
    let mut violations = Vec::new();
    require_nonempty(
        &mut violations,
        "projection_id",
        envelope.projection_id.as_str(),
    );
    require_nonempty(
        &mut violations,
        "projection_version",
        envelope.projection_version.as_str(),
    );
    require_nonempty(
        &mut violations,
        "source_request_id",
        envelope.source_request_id.as_str(),
    );
    require_nonempty(
        &mut violations,
        "provenance.runtime",
        envelope.provenance.runtime.as_str(),
    );
    let external_source = envelope.provenance.source_version_status
        == ProjectionSourceVersionStatus::ExternalAdapterCapture;
    let expected_source = if external_source {
        EXTERNAL_ADAPTER_CAPTURE_PROJECTION_ID
    } else {
        RUN_TIMELINE_PROJECTION_ID
    };
    require_eq(
        &mut violations,
        "provenance.source_projection_id",
        envelope.provenance.source_projection_id.as_str(),
        expected_source,
    );
    if external_source && envelope.source_request_doc_id.is_some() {
        violations
            .push("external adapter capture must not claim source_request_doc_id".to_string());
    }
    for (index, reference) in envelope.provenance.rendered_request_refs.iter().enumerate() {
        require_nonempty(
            &mut violations,
            &format!("provenance.rendered_request_refs[{index}].capture_doc_id"),
            &reference.capture_doc_id,
        );
        require_nonempty(
            &mut violations,
            &format!("provenance.rendered_request_refs[{index}].request_doc_id"),
            &reference.request_doc_id,
        );
        require_nonempty(
            &mut violations,
            &format!("provenance.rendered_request_refs[{index}].request_commit_cid"),
            &reference.request_commit_cid,
        );
        if envelope.source_request_doc_id.as_deref() != Some(reference.request_doc_id.as_str()) {
            violations.push(format!(
                "provenance.rendered_request_refs[{index}].request_doc_id does not match source_request_doc_id"
            ));
        }
    }
    require_eq(
        &mut violations,
        "projection_id",
        envelope.projection_id.as_str(),
        envelope.output.kind().id(),
    );
    require_eq(
        &mut violations,
        "projection_version",
        envelope.projection_version.as_str(),
        ADAPTER_PROJECTION_VERSION,
    );

    match &envelope.output {
        AdapterProjection::AtifTrajectory(projection) => {
            atif::validate_atif_trajectory(&mut violations, projection);
        }
        AdapterProjection::OpenAiCodexRunTrace(projection) => {
            validate_openai_codex_projection(&mut violations, projection);
        }
        AdapterProjection::LangGraphStateHistory(projection) => {
            validate_langgraph_projection(&mut violations, projection);
        }
        AdapterProjection::MultiAgentTask(projection) => {
            validate_multi_agent_projection(&mut violations, projection);
        }
    }

    if violations.is_empty() {
        Ok(())
    } else {
        Err(AdapterProjectionContractError { violations })
    }
}

fn validate_openai_codex_projection(
    violations: &mut Vec<String>,
    projection: &OpenAiCodexRunTraceProjection,
) {
    require_nonempty(violations, "output.projection.run_id", &projection.run_id);
    require_nonempty_vec(
        violations,
        "output.projection.items",
        projection.items.len(),
    );
    for (index, item) in projection.items.iter().enumerate() {
        match item {
            OpenAiCodexTraceItem::Request { id, .. } | OpenAiCodexTraceItem::Message { id, .. } => {
                require_nonempty(violations, &format!("items[{index}].id"), id);
            }
            OpenAiCodexTraceItem::ToolCall {
                id, name, status, ..
            } => {
                require_nonempty(violations, &format!("items[{index}].id"), id);
                require_nonempty(violations, &format!("items[{index}].name"), name);
                require_nonempty(violations, &format!("items[{index}].status"), status);
            }
        }
    }
}

fn validate_langgraph_projection(
    violations: &mut Vec<String>,
    projection: &LangGraphStateHistoryProjection,
) {
    require_nonempty(
        violations,
        "output.projection.checkpoint_id",
        &projection.checkpoint_id,
    );
    require_nonempty(
        violations,
        "output.projection.root_request_id",
        &projection.root_request_id,
    );
    require_nonempty_vec(
        violations,
        "output.projection.nodes",
        projection.nodes.len(),
    );
    if !projection.values.contains_key("request_id") {
        violations.push("output.projection.values.request_id is required".to_string());
    }
    for (index, node) in projection.nodes.iter().enumerate() {
        require_nonempty(violations, &format!("nodes[{index}].id"), &node.id);
        require_nonempty(violations, &format!("nodes[{index}].kind"), &node.kind);
    }
    for (index, edge) in projection.edges.iter().enumerate() {
        require_nonempty(violations, &format!("edges[{index}].from"), &edge.from);
        require_nonempty(violations, &format!("edges[{index}].to"), &edge.to);
        require_nonempty(violations, &format!("edges[{index}].kind"), &edge.kind);
    }
    for (index, task) in projection.tasks.iter().enumerate() {
        require_nonempty(violations, &format!("tasks[{index}].id"), &task.id);
        require_nonempty(violations, &format!("tasks[{index}].name"), &task.name);
        require_nonempty(violations, &format!("tasks[{index}].status"), &task.status);
    }
}

fn validate_multi_agent_projection(
    violations: &mut Vec<String>,
    projection: &MultiAgentTaskProjection,
) {
    require_nonempty(violations, "output.projection.task_id", &projection.task_id);
    require_nonempty_vec(
        violations,
        "output.projection.participants",
        projection.participants.len(),
    );
    for (index, participant) in projection.participants.iter().enumerate() {
        require_nonempty(
            violations,
            &format!("participants[{index}].role"),
            &participant.role,
        );
        if participant
            .node_did
            .as_deref()
            .unwrap_or("")
            .trim()
            .is_empty()
            && participant
                .agent_id
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty()
        {
            violations.push(format!(
                "participants[{index}] must include node_did or agent_id"
            ));
        }
    }
    for (index, message) in projection.messages.iter().enumerate() {
        require_nonempty(violations, &format!("messages[{index}].id"), &message.id);
        require_nonempty(
            violations,
            &format!("messages[{index}].role"),
            &message.role,
        );
    }
    for (index, delegation) in projection.delegations.iter().enumerate() {
        require_nonempty(
            violations,
            &format!("delegations[{index}].parent_request_id"),
            &delegation.parent_request_id,
        );
        require_nonempty(
            violations,
            &format!("delegations[{index}].child_request_id"),
            &delegation.child_request_id,
        );
    }
    for (index, event) in projection.tool_events.iter().enumerate() {
        require_nonempty(violations, &format!("tool_events[{index}].id"), &event.id);
        require_nonempty(
            violations,
            &format!("tool_events[{index}].tool_name"),
            &event.tool_name,
        );
        require_nonempty(
            violations,
            &format!("tool_events[{index}].status"),
            &event.status,
        );
    }
}

fn require_nonempty(violations: &mut Vec<String>, path: &str, value: &str) {
    if value.trim().is_empty() {
        violations.push(format!("{path} is required"));
    }
}

fn require_nonempty_vec(violations: &mut Vec<String>, path: &str, len: usize) {
    if len == 0 {
        violations.push(format!("{path} must not be empty"));
    }
}

fn require_eq(violations: &mut Vec<String>, path: &str, actual: &str, expected: &str) {
    if actual != expected {
        violations.push(format!("{path} expected {expected:?}, got {actual:?}"));
    }
}
