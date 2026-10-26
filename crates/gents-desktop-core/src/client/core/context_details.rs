use std::collections::VecDeque;

use gents::toolset::{LastRequestContextSnapshot, SessionContextDetails};
use tokio::sync::Mutex;

use super::{ClientCore, ConfigAccess};

const MAX_CACHED_CALLS: usize = 64;

#[derive(PartialEq, Eq)]
struct ContextDetailsKey {
    node_did: String,
    requester_did: Option<String>,
    session_id: String,
    request_id: String,
    request_doc_id: String,
    call_id: String,
    turn_index: usize,
    attempt: u32,
}

#[derive(Default)]
pub(super) struct ContextDetailsCache {
    entries: Mutex<VecDeque<(ContextDetailsKey, SessionContextDetails)>>,
}

impl ClientCore {
    /// The caller must first read the selected call through the current authorized
    /// session scope. Captures are immutable per call; only successful numerical
    /// breakdowns are cached, so missing replicas or denied reads can be retried.
    pub async fn session_context_details(
        &self,
        access: &ConfigAccess,
        node_did: &str,
        requester_did: Option<&str>,
        session_id: &str,
        request_doc_id: &str,
        context: &LastRequestContextSnapshot,
    ) -> anyhow::Result<Option<SessionContextDetails>> {
        let key = ContextDetailsKey {
            node_did: node_did.into(),
            requester_did: requester_did.map(str::to_owned),
            session_id: session_id.into(),
            request_id: context.request_id.clone(),
            request_doc_id: request_doc_id.into(),
            call_id: context.call_id.clone(),
            turn_index: context.accounting.turn_index,
            attempt: context.accounting.attempt,
        };
        {
            let mut entries = self.context_details.entries.lock().await;
            if let Some(index) = entries.iter().position(|(cached, _)| cached == &key) {
                let entry = entries.remove(index).expect("located cache entry");
                let details = entry.1.clone();
                entries.push_back(entry);
                return Ok(Some(details));
            }
        }
        let details = gents::toolset::load_session_context_details(
            access,
            node_did,
            requester_did,
            session_id,
            context,
        )
        .await?;
        if let Some(details) = &details {
            let mut entries = self.context_details.entries.lock().await;
            entries.retain(|(cached, _)| cached != &key);
            if entries.len() == MAX_CACHED_CALLS {
                entries.pop_front();
            }
            entries.push_back((key, details.clone()));
        }
        Ok(details)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{ClientCoreOptions, DesktopPaths};
    use serde_json::json;
    use wiremock::{
        matchers::{body_string_contains, method},
        Mock, MockServer, ResponseTemplate,
    };

    fn fixture() -> (serde_json::Value, LastRequestContextSnapshot) {
        let body = json!({"messages":[{"role":"user","content":"Hello"}]});
        let tokens = serde_json::to_vec(&body["messages"]).unwrap().len() / 4;
        let accounting = serde_json::from_value(json!({
            "accounting_version":gents_protocol::rendered_request::CONTEXT_ACCOUNTING_VERSION,
            "turn_index":0, "attempt":0, "estimator":"openai_chat_wire_json_bytes_div_4_v1",
            "components":{"messages":tokens,"documents":0,"tool_schemas":0,"additional_parameters":0,"output_schema":0},
            "estimated_input_tokens":tokens, "context_window":10000,
            "compaction_threshold_basis_points":7500,"compaction_threshold_tokens":7500,
            "compaction_reason":"below_threshold"
        })).unwrap();
        (
            body,
            LastRequestContextSnapshot {
                request_id: "request".into(),
                call_id: "call".into(),
                call_sequence: 1,
                queued_at: None,
                accounting,
            },
        )
    }

    async fn mounts(server: &MockServer, body: &serde_json::Value, present: bool, expected: u64) {
        Mock::given(method("POST"))
            .and(body_string_contains("AgentRequest("))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!({"data":{"AgentRequest":[{"_docID":"physical-request"}]}}),
                ),
            )
            .expect(expected)
            .mount(server)
            .await;
        let captures = if present {
            json!([{
                "capture_version":1,"source":"openai_chat_completions",
                "request_json":body.to_string(),"provenance_json":json!({"admission":{"call_id":"call"}}).to_string()
            }])
        } else {
            json!([])
        };
        Mock::given(method("POST"))
            .and(body_string_contains("RenderedRequest("))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"data":{"RenderedRequest":captures}})),
            )
            .expect(expected)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn context_details_cache_reuses_only_the_same_call_over_http() {
        let directory = tempfile::tempdir().unwrap();
        let core = ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(directory.path()),
            ClientCoreOptions::local_only(),
        )
        .await
        .unwrap();
        let core = std::sync::Arc::new(core);
        let server = MockServer::start().await;
        let access = ConfigAccess::graphql(server.uri());
        let (body, mut context) = fixture();
        mounts(&server, &body, true, 2).await;
        let mut unsupported = context.clone();
        unsupported.accounting.estimator = "unsupported".into();
        assert!(core
            .session_context_details(
                &access,
                "node",
                None,
                "session",
                "physical-request",
                &unsupported
            )
            .await
            .unwrap()
            .is_none());
        assert!(server.received_requests().await.unwrap().is_empty());
        let first = core
            .session_context_details(
                &access,
                "node",
                None,
                "session",
                "physical-request",
                &context,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            core.session_context_details(
                &access,
                "node",
                None,
                "session",
                "physical-request",
                &context
            )
            .await
            .unwrap(),
            Some(first)
        );
        context.call_id = "different-call".into();
        assert!(core
            .session_context_details(
                &access,
                "node",
                None,
                "session",
                "physical-request",
                &context
            )
            .await
            .unwrap()
            .is_none());
        core.shutdown().await.unwrap();
        server.verify().await;
    }

    #[tokio::test]
    async fn context_details_cache_retries_a_capture_that_was_not_replicated_yet() {
        let directory = tempfile::tempdir().unwrap();
        let core = ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(directory.path()),
            ClientCoreOptions::local_only(),
        )
        .await
        .unwrap();
        let core = std::sync::Arc::new(core);
        let server = MockServer::start().await;
        let access = ConfigAccess::graphql(server.uri());
        let (body, context) = fixture();
        mounts(&server, &body, false, 1).await;
        assert!(core
            .session_context_details(
                &access,
                "node",
                None,
                "session",
                "physical-request",
                &context
            )
            .await
            .unwrap()
            .is_none());
        server.verify().await;
        server.reset().await;
        mounts(&server, &body, true, 1).await;
        assert!(core
            .session_context_details(
                &access,
                "node",
                None,
                "session",
                "physical-request",
                &context
            )
            .await
            .unwrap()
            .is_some());
        core.shutdown().await.unwrap();
        server.verify().await;
    }
}
