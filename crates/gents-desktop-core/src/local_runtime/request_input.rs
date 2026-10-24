use gents_protocol::goal_control::STATUS_GOAL_STOP_CONTRACT_FIELD;
use gents_protocol::request_input::{RequestInput, STATUS_INPUT_CONTRACT_FIELD};
use gents_protocol::serve_lifecycle::ObservedServeLifecycle;
use serde_json::Value;

use crate::client::PeerRecord;

use super::{http::peer_status, serving_runtime, LocalRuntimeError, Result};

pub(crate) async fn ensure_request_input_supported(
    record: Option<&PeerRecord>,
    input: &RequestInput,
) -> Result<()> {
    input.validate_attachments()?;
    if input.captured_context.is_empty()
        && input.attachments.is_empty()
        && input.goal_assignment.is_none()
    {
        return Ok(());
    }
    if !input
        .captured_context
        .iter()
        .all(|context| context.is_well_formed())
    {
        return Err(LocalRuntimeError::InvalidAttachedContext);
    }
    let Some(record) = record else {
        return Ok(());
    };
    let status = peer_status(record)
        .await?
        .ok_or(LocalRuntimeError::RequestInputUnverifiable)?;
    decode(&status, &record.node_did, input)
}

fn decode(status: &Value, expected_did: &str, input: &RequestInput) -> Result<()> {
    if !matches!(
        serving_runtime(status, expected_did)?,
        ObservedServeLifecycle::Ready
    ) {
        return Err(LocalRuntimeError::RuntimeNotReady {
            action: "sending this message",
        });
    }
    if !input.goal_assignment_compatible(
        status
            .get(STATUS_GOAL_STOP_CONTRACT_FIELD)
            .and_then(Value::as_u64),
    ) {
        return Err(LocalRuntimeError::GoalControlContractMismatch);
    }
    if !input.captured_input_compatible(
        status
            .get(STATUS_INPUT_CONTRACT_FIELD)
            .and_then(Value::as_u64),
    ) {
        return Err(LocalRuntimeError::InputContractUnsupported);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_protocol::request_input::REQUEST_INPUT_CONTRACT_VERSION;
    use serde_json::json;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    fn captured_input() -> RequestInput {
        serde_json::from_value(json!({"captured_context": [{
            "kind": "file", "source": "editor", "capturedAt": "2026-10-20T00:00:00Z",
            "uri": "file:///workspace/context.rs", "terminalName": null,
            "workspaceUri": "file:///workspace", "version": 2, "unsaved": true,
            "range": null, "content": "original unsaved snapshot"
        }]}))
        .unwrap()
    }

    #[tokio::test]
    async fn captured_input_requires_the_selected_ready_runtime_contract() {
        let server = MockServer::start().await;
        let mut record = PeerRecord::new("Remote", "iroh://peer", "did:key:selected");
        record.graphql = Some(format!("{}/api/v0/graphql", server.uri()));
        let input = captured_input();
        let supported = json!({
            "node_did": record.node_did, "lifecycle": "ready",
            "request_input_contract_version": REQUEST_INPUT_CONTRACT_VERSION
        });
        for (field, value) in [
            ("request_input_contract_version", Value::Null),
            (
                "request_input_contract_version",
                json!(REQUEST_INPUT_CONTRACT_VERSION + 1),
            ),
            ("request_input_contract_version", json!("1")),
            ("node_did", json!("did:key:other")),
            ("lifecycle", json!("starting")),
            ("lifecycle", Value::Null),
        ] {
            let mut status = supported.clone();
            status[field] = value;
            server.reset().await;
            Mock::given(method("GET"))
                .and(path("/status"))
                .respond_with(ResponseTemplate::new(200).set_body_json(status))
                .expect(1)
                .mount(&server)
                .await;
            assert!(
                ensure_request_input_supported(Some(&record), &input)
                    .await
                    .is_err(),
                "{field}"
            );
            server.verify().await;
        }
        server.reset().await;
        Mock::given(method("GET"))
            .and(path("/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(supported))
            .expect(1)
            .mount(&server)
            .await;
        ensure_request_input_supported(Some(&record), &input)
            .await
            .unwrap();
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(!requests[0].headers.contains_key("authorization"));
        assert!(requests[0].body.is_empty());
    }

    #[tokio::test]
    async fn plain_input_does_not_require_an_advertisement_or_an_http_read() {
        let record = PeerRecord::new("Remote", "http://127.0.0.1:1", "did:key:selected");
        ensure_request_input_supported(Some(&record), &RequestInput::default())
            .await
            .unwrap();
        ensure_request_input_supported(None, &captured_input())
            .await
            .unwrap();
        let record = PeerRecord::new("Remote", "iroh://peer", "did:key:selected");
        assert!(
            ensure_request_input_supported(Some(&record), &captured_input())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn malformed_captures_are_rejected_before_contacting_the_runtime() {
        let record = PeerRecord::new("Remote", "http://127.0.0.1:1", "did:key:selected");
        let mut input = captured_input();
        input.captured_context[0].version = Some(0);
        let error = ensure_request_input_supported(Some(&record), &input)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("Remove it and capture it again"));
    }

    #[test]
    fn assigned_requests_require_matching_goal_contract_without_requiring_captures() {
        use gents_protocol::goal_control::GOAL_STOP_CONTRACT_VERSION;
        let input = RequestInput {
            goal_assignment: Some(gents_protocol::request_input::GoalAssignmentInput {
                goal_id: "new-assignment".into(),
                root_request_doc_id: Some("opening-document".into()),
            }),
            ..Default::default()
        };
        let mut status = json!({"node_did":"did:key:selected", "lifecycle":"ready"});
        assert!(decode(&status, "did:key:selected", &input).is_err());
        status[STATUS_GOAL_STOP_CONTRACT_FIELD] = json!(GOAL_STOP_CONTRACT_VERSION - 1);
        assert!(decode(&status, "did:key:selected", &input).is_err());
        status[STATUS_GOAL_STOP_CONTRACT_FIELD] = json!(GOAL_STOP_CONTRACT_VERSION);
        decode(&status, "did:key:selected", &input).unwrap();
        assert!(decode(&status, "did:key:other", &input).is_err());
        status["lifecycle"] = json!("starting");
        assert!(decode(&status, "did:key:selected", &input).is_err());
    }

    #[tokio::test]
    async fn assignment_only_input_probes_the_selected_runtime_before_publication() {
        use gents_protocol::goal_control::GOAL_STOP_CONTRACT_VERSION;
        let server = MockServer::start().await;
        let mut record = PeerRecord::new("Remote", "iroh://selected", "did:key:selected");
        record.graphql = Some(format!("{}/api/v0/graphql", server.uri()));
        let input = RequestInput {
            goal_assignment: Some(gents_protocol::request_input::GoalAssignmentInput {
                goal_id: "assignment".into(),
                root_request_doc_id: Some("opening-document".into()),
            }),
            ..Default::default()
        };
        for supported in [false, true] {
            server.reset().await;
            Mock::given(method("GET"))
                .and(path("/status"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "node_did":"did:key:selected", "lifecycle":"ready",
                    (STATUS_GOAL_STOP_CONTRACT_FIELD): if supported {
                        GOAL_STOP_CONTRACT_VERSION
                    } else {
                        GOAL_STOP_CONTRACT_VERSION - 1
                    }
                })))
                .expect(1)
                .mount(&server)
                .await;
            assert_eq!(
                ensure_request_input_supported(Some(&record), &input)
                    .await
                    .is_ok(),
                supported
            );
            server.verify().await;
            let requests = server.received_requests().await.unwrap();
            assert_eq!(requests.len(), 1);
            assert!(!requests[0].headers.contains_key("authorization"));
            assert!(requests[0].body.is_empty());
        }
    }
}
