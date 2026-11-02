use gents_protocol::row::AgentRequestRow;

pub(crate) fn sender_header(
    node: &str,
    agent: &str,
    session: &str,
    request: &str,
    at: &str,
) -> String {
    let abbreviated = match (node.get(..16), node.get(node.len().saturating_sub(4)..)) {
        (Some(head), Some(tail)) if node.len() > 20 => format!("{head}..{tail}"),
        _ => node.to_owned(),
    };
    let agent = agent.strip_prefix(&format!("{node}:")).unwrap_or(agent);
    format!(
        "[message agent:{agent} session:{session} node:{abbreviated} · at:{at} · cause:{request}]"
    )
}

/// Display extraction needs the readable canonical parent and an exact generated
/// envelope. Missing context stays unavailable; provider-facing signed bytes are unchanged.
pub fn session_message_display_body<'a>(
    request: &'a AgentRequestRow,
    parent: &AgentRequestRow,
) -> Option<&'a str> {
    session_message_display_body_with_content(request, parent, request.content.as_deref()?)
}

pub(crate) fn session_message_display_body_with_content<'a>(
    request: &AgentRequestRow,
    parent: &AgentRequestRow,
    content: &'a str,
) -> Option<&'a str> {
    let super::RequestOrigin::SessionMessage {
        parent_request_doc_id: Some(document),
    } = super::request_origin(request)
    else {
        return None;
    };
    if parent.doc_id.as_deref() != Some(document)
        || request.caused_by_parent_request_id.as_deref() != Some(parent.request_id.as_str())
        || request.requester_did.as_deref() != parent.node_did.as_deref()
    {
        return None;
    }
    fn present(value: Option<&str>) -> Option<&str> {
        value.filter(|text| !text.trim().is_empty())
    }
    let header = sender_header(
        present(parent.node_did.as_deref())?,
        present(parent.agent_id.as_deref())?,
        present(parent.session_id.as_deref())?,
        &parent.request_id,
        present(request.created_at.as_deref())?,
    );
    content.strip_prefix(&format!("{header}\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rows() -> (AgentRequestRow, AgentRequestRow) {
        let parent = AgentRequestRow {
            doc_id: Some("parent-document".into()),
            request_id: "parent-request".into(),
            node_did: Some("did:test:sender".into()),
            agent_id: Some("did:test:sender:engineer".into()),
            session_id: Some("parent-session".into()),
            ..Default::default()
        };
        let request = AgentRequestRow {
            request_id: "received-request".into(),
            requester_did: Some("did:test:sender".into()),
            created_at: Some("2026-10-23T14:00:00Z".into()),
            caused_by_parent_request_doc_id: parent.doc_id.clone(),
            caused_by_parent_request_id: Some(parent.request_id.clone()),
            caused_by_parent_tool_call_doc_id: Some("tool-document".into()),
            content: Some("[message agent:engineer session:parent-session node:did:test:sender · at:2026-10-23T14:00:00Z · cause:parent-request]\n\nInspect the layout.\nKeep the draft.".into()),
            ..Default::default()
        };
        (request, parent)
    }
    #[test]
    fn exact_native_envelope_projects_only_the_received_body() {
        let (request, parent) = rows();
        let original = request.content.clone();
        assert_eq!(
            session_message_display_body(&request, &parent),
            Some("Inspect the layout.\nKeep the draft.")
        );
        assert_eq!(request.content, original);
    }
    #[test]
    fn person_text_that_looks_like_an_envelope_is_not_unwrapped() {
        let (mut request, parent) = rows();
        request.caused_by_parent_tool_call_doc_id = None;
        assert_eq!(session_message_display_body(&request, &parent), None);
    }
    #[test]
    fn mismatched_parent_scope_never_supplies_a_body() {
        let (request, parent) = rows();
        for field in ["document", "request", "node", "agent", "session"] {
            let mut changed = parent.clone();
            match field {
                "document" => changed.doc_id = Some("other".into()),
                "request" => changed.request_id = "other".into(),
                "node" => changed.node_did = Some("did:test:other".into()),
                "agent" => changed.agent_id = Some("other".into()),
                _ => changed.session_id = Some("other".into()),
            }
            assert_eq!(
                session_message_display_body(&request, &changed),
                None,
                "{field}"
            );
        }
    }
    #[test]
    fn absent_parent_fields_and_noncanonical_envelopes_remain_unavailable() {
        let (request, mut parent) = rows();
        parent.agent_id = None;
        assert_eq!(session_message_display_body(&request, &parent), None);
        let (mut request, parent) = rows();
        request.created_at = Some("2026-10-23T14:00:01Z".into());
        assert_eq!(session_message_display_body(&request, &parent), None);
        request.content = Some("legacy received text".into());
        assert_eq!(session_message_display_body(&request, &parent), None);
    }
    #[test]
    fn an_inner_literal_header_and_whitespace_remain_part_of_the_body() {
        let (mut request, parent) = rows();
        request
            .content
            .as_mut()
            .unwrap()
            .push_str("\n\n[message agent:literal]\n  ");
        assert_eq!(
            session_message_display_body(&request, &parent),
            Some("Inspect the layout.\nKeep the draft.\n\n[message agent:literal]\n  ")
        );
    }
    #[test]
    fn same_node_abbreviation_is_shared_with_materialization() {
        assert_eq!(sender_header("did:key:123456789012345678901234", "did:key:123456789012345678901234:engineer", "session", "cause", "at"), "[message agent:engineer session:session node:did:key:12345678..1234 · at:at · cause:cause]");
    }
}
