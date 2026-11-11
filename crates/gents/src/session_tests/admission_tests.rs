use std::sync::Arc;
use super::AgentRequestAdmissionVerifier;
use gents_p2p::p2p_reconcile::enrollment_authority_channel;
use gents_protocol::request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate};
use gents_store::identity::{KeyIdentity, NodeIdentity};
use gents_store::schema::ensure_runtime_schemas;

    #[derive(Clone)]
    struct WarnCapture(Arc<std::sync::Mutex<String>>);

    #[test]
    fn version_skewed_refused_row_keeps_its_refusal_and_warns_on_the_skipped_projection() {
        crate::test_support::root::enable_scoped_event_capture();
        let captured = Arc::new(std::sync::Mutex::new(String::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .with_writer(WarnCapture(captured.clone()))
            .finish();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        tracing::subscriber::with_default(subscriber, || {
            runtime.block_on(async {
                let fixture = AdmissionFixture::new("version-skew").await;
                let agent_did = fixture.agent.did().to_owned();
                let escaped_agent_did = gents_store::graphql::escape_graphql_string(&agent_did);
                let node_did = gents_store::graphql::escape_graphql_string(fixture.runtime.did());
                let doc_id = fixture
                    .write_as_runtime(
                        format!(
                            r#"mutation {{ create_AgentRequest(input: {{
                            request_id: "version-skewed", purpose: "normal", node_did: "{node_did}",
                            requester_did: "{node_did}", agent_did: "{escaped_agent_did}",
                            session_id: "skew-session", content: "run the disallowed skill",
                            lifecycle_state: "pending", created_at: "2026-09-01T00:00:00Z",
                            input: {{ future_field: "written by a newer runtime" }},
                            retry_count: 0, max_retries: 3, request_hop: 0
                        }}) {{ _docID }} }}"#
                        ),
                        "AgentRequest",
                    )
                    .await;
                fixture.grant_working_agent(&doc_id, &agent_did).await;

                gents_session::request::admission::terminalize_pending_request_rejection(
                    fixture.node(),
                    &doc_id,
                    fixture.runtime.did(),
                    &agent_did,
                    fixture.agent.as_ref(),
                    "request admission denied: request activates a skill outside its context allowlist",
                    "test.admission_rejection_version_skew",
                )
                .await
                .unwrap();

                let selected = fixture
                    .node()
                    .execute(&format!(
                        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state }} AgentSession(filter: {{ session_id: {{ _eq: "skew-session" }} }}) {{ _docID }} }}"#,
                        gents_store::graphql::escape_graphql_string(&doc_id),
                    ))
                    .await;
                assert!(!selected.has_errors(), "{:?}", selected.errors);
                let data = selected.data.as_ref().unwrap();
                assert_eq!(data["AgentRequest"][0]["lifecycle_state"], "failed");
                assert!(
                    data["AgentSession"].as_array().unwrap().is_empty(),
                    "an undecodable row mints no session"
                );
                fixture
                    .db
                    .node
                    .shutdown()
                    .await
                    .expect("shut down test node");
            });
        });
        let logs = captured.lock().unwrap().clone();
        assert!(
            logs.contains("refused row skipped session projection"),
            "the version-skew skip must be observable, log: {logs}"
        );
    }

    impl std::io::Write for WarnCapture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .unwrap()
                .push_str(&String::from_utf8_lossy(buf));
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for WarnCapture {
        type Writer = WarnCapture;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }
