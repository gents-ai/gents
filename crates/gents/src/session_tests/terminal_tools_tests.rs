use gents_protocol::row::AgentRequestRow;
use gents_session::request::lifecycle::terminal_tools::*;
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;

async fn account_request_tools(
    access: &ConfigAccess,
    request_doc: &str,
    session: &str,
    generation: &str,
    normal: bool,
) -> anyhow::Result<()> {
    let request_doc = request_doc.to_owned();
    let session = session.to_owned();
    let generation = generation.to_owned();
    access
        .transact_write("test.plugin_child_accounting", |txn| {
            let request_doc = request_doc.clone();
            let session = session.clone();
            let generation = generation.clone();
            Box::pin(async move {
                let response = txn
                    .execute(&format!(
                        r#"{{ AgentRequest(docID: "{}") {{ {} }} }}"#,
                        escape_graphql_string(&request_doc),
                        gents_protocol::row::AGENT_REQUEST_FIELDS,
                    ))
                    .await?;
                let request: AgentRequestRow =
                    serde_json::from_value(response["data"]["AgentRequest"][0].clone())?;
                // Transcript rows are scoped by the request's node and
                // requester, not by the Agent that reads them.
                let node_did = request
                    .node_did
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("request has no node"))?;
                let requester = request.requester_did.clone();
                let headers = gents_session::session::load_request_headers_in_txn(
                    txn,
                    &session,
                    &node_did,
                    requester.as_deref(),
                    &request_doc,
                )
                .await?;
                let mut reader = gents_session::session::TxnCanonicalReader::new(
                    txn,
                    &node_did,
                    requester.as_deref(),
                );
                reader.observe_headers(&headers);
                account_tools_in_txn(
                    txn,
                    &mut reader,
                    &request,
                    &headers,
                    &generation,
                    normal,
                    &chrono::Utc::now().to_rfc3339(),
                )
                .await
            })
        })
        .await
}

#[tokio::test]
async fn generated_plugin_child_request_accounting_matches_execution_owner() {
    for case in gents_lean_vocab::families::plugin_effects_child_accounting::cases() {
        let name = case.input.name.as_str();
        let (fixture, request_owner) =
            gents_toolcalls::call::lifecycle::admission_fixture::published_admission_with_owner(
                gents_toolcalls::call::lifecycle::admission_fixture::PublishedAdmissionOptions {
                    name: format!("account-{name}"),
                    composition: gents_automation::lifecycle_hooks::composition(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let gents_toolcalls::call::lifecycle::admission_fixture::PublishedAdmission {
            node,
            path,
            tool: mut parent,
            ..
        } = fixture;
        if name.starts_with("background_parent") {
            parent.background().await.unwrap();
        }
        let mut child = parent
            .admit_plugin_effect(1, "fixture_effect", "{}")
            .await
            .unwrap();
        if !name.contains("pending") {
            child.start_running().await.unwrap();
        }
        if name.starts_with("timed_out") {
            child.timeout().await.unwrap();
        } else if name.starts_with("completed") {
            child
                .complete_raw_with_presentation(
                    "child done",
                    "child done",
                    gents_protocol::output::PayloadPresentation::Full,
                )
                .await
                .unwrap();
        }
        let child_doc = child.doc_id().unwrap().to_owned();
        let generation = parent.execution_generation().unwrap().to_owned();
        let request_doc = parent.request_doc_id().unwrap().to_owned();
        let session = parent.session_id().to_owned();
        if !case.input.exceptional {
            parent
                .complete("parent completed before child")
                .await
                .unwrap();
        }
        let access = ConfigAccess::local_with_optional_agent_signer(
            node.clone(),
            request_owner.actor_did().unwrap(),
            request_owner.managed_agent_signer(),
        )
        .unwrap();
        let accounted = account_request_tools(
            &access,
            &request_doc,
            &session,
            &generation,
            !case.input.exceptional,
        )
        .await;
        let response = access.execute(&format!(
                r#"{{ AgentToolCall(docID: "{}") {{ lifecycle_state started_at stuck_since terminal_output }} }}"#,
                escape_graphql_string(&child_doc),
            )).await.unwrap();
        let row = response["data"]["AgentToolCall"][0].clone();
        let normal_ready = if case.input.exceptional {
            accounted.unwrap();
            account_request_tools(&access, &request_doc, &session, &generation, true)
                .await
                .is_ok()
        } else {
            accounted.is_ok()
        };
        case.check(
            gents_lean_vocab::families::plugin_effects_child_accounting::AccountingOutput {
                state: row["lifecycle_state"].as_str().unwrap().to_owned(),
                started: !row["started_at"].is_null(),
                stuck: !row["stuck_since"].is_null(),
                terminal_output: !row["terminal_output"].is_null(),
                normal_ready,
            },
        );
        drop(request_owner);
        node.shutdown().await.expect("shut down test node");
        let _ = std::fs::remove_dir_all(path);
    }
}
