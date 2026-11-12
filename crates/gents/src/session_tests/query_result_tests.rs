use gents_protocol::output::{MessagePublication, MessageRole, PresentedPayload};
use gents_session::session::tool_call_state::ToolCallState;
use gents_session::tool_call::query::result::*;
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;

#[tokio::test]
async fn generated_plugin_child_terminal_presentation_matches_execution_owner() {
    use gents_protocol::output::{PayloadPresentation, PresentationPart};
    let snapshot = gents_lean_vocab::lean_contract_snapshot();
    let cases = snapshot
        .plugin_resource_tool_effect_terminal_cases
        .as_array()
        .expect("generated plugin terminal cases");
    for (index, case) in cases.iter().enumerate() {
        let (fixture, request_owner) =
            gents_toolcalls::call::lifecycle::admission_fixture::published_admission_with_owner(
                gents_toolcalls::call::lifecycle::admission_fixture::PublishedAdmissionOptions {
                    name: format!("effect-terminal-{index}"),
                    composition: gents_automation::lifecycle_hooks::composition(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let gents_toolcalls::call::lifecycle::admission_fixture::PublishedAdmission {
            node,
            path,
            tool: parent,
            ..
        } = fixture;
        let mut child = parent
            .admit_plugin_effect(1, "fixture_effect", "{}")
            .await
            .unwrap();
        child.start_running().await.unwrap();
        let presentation = PayloadPresentation::Composed {
            parts: vec![
                PresentationPart::Literal {
                    text: "error: ".into(),
                },
                PresentationPart::OutputRange {
                    start_byte: 1,
                    end_byte: 3,
                },
            ],
        };
        let mut replay = gents_toolcalls::call::lifecycle::ToolCallLifecycle::load_by_doc_id_as(
            node.clone(),
            child.actor().clone(),
            child.doc_id().unwrap(),
            parent.node_did(),
            parent.session_id(),
            parent.requester_did(),
        )
        .await
        .unwrap()
        .unwrap();
        replay.inherit_signer(&child).unwrap();
        child
            .complete_raw_with_presentation("abc", "error: bc", presentation.clone())
            .await
            .unwrap();
        let accepted = if case["replay"].as_bool().unwrap() {
            let conflict = case["conflict"].as_bool().unwrap();
            replay
                .complete_raw_with_presentation(
                    "abc",
                    if conflict { "abc" } else { "error: bc" },
                    if conflict {
                        PayloadPresentation::Full
                    } else {
                        presentation
                    },
                )
                .await
                .is_ok()
        } else {
            load_tool_call_presentation(
                &child.access().unwrap(),
                child.doc_id().unwrap(),
                parent.node_did(),
                parent.session_id(),
                parent.requester_did(),
            )
            .await
            .unwrap()
            .result
            .as_deref()
                == Some("error: bc")
        };
        assert_eq!(
            accepted,
            case["expected"].as_bool().unwrap(),
            "case {index}"
        );
        drop(request_owner);
        node.shutdown().await.expect("shut down test node");
        let _ = std::fs::remove_dir_all(path);
    }
}

#[tokio::test]
async fn plugin_child_reads_canonical_sources_after_parent_terminal_without_transcript() {
    let (fixture, request_owner) =
        gents_toolcalls::call::lifecycle::admission_fixture::published_admission_with_owner(
            gents_toolcalls::call::lifecycle::admission_fixture::PublishedAdmissionOptions {
                name: "plugin-child-reader".to_owned(),
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
    let arguments = r#"{"value":"input"}"#;
    let mut child = parent
        .admit_plugin_effect(1, "fixture_effect", arguments)
        .await
        .unwrap();
    let child_doc = child.doc_id().unwrap().to_owned();
    let node_did = parent.node_did().to_owned();
    let session_id = parent.session_id().to_owned();
    let requester = parent.requester_did().map(str::to_owned);
    let access = ConfigAccess::local_as(node.clone(), child.actor().clone());
    assert_eq!(
        load_tool_call_arguments(
            &access,
            &child_doc,
            &node_did,
            &session_id,
            requester.as_deref()
        )
        .await
        .unwrap(),
        arguments
    );
    child.start_running().await.unwrap();
    let mut replay = gents_toolcalls::call::lifecycle::ToolCallLifecycle::load_by_doc_id_as(
        node.clone(),
        child.actor().clone(),
        &child_doc,
        &node_did,
        &session_id,
        requester.as_deref(),
    )
    .await
    .unwrap()
    .unwrap();
    replay.inherit_signer(&child).unwrap();
    child
        .complete_raw_with_presentation(
            "effect result",
            "[bounded effect]",
            gents_protocol::output::PayloadPresentation::Composed {
                parts: vec![gents_protocol::output::PresentationPart::Literal {
                    text: "[bounded effect]".into(),
                }],
            },
        )
        .await
        .unwrap();
    assert!(!replay
        .complete_raw_with_presentation(
            "effect result",
            "[bounded effect]",
            gents_protocol::output::PayloadPresentation::Composed {
                parts: vec![gents_protocol::output::PresentationPart::Literal {
                    text: "[bounded effect]".into()
                }]
            },
        )
        .await
        .unwrap());
    replay.set_state(ToolCallState::Running);
    assert!(replay
        .complete_raw_with_presentation(
            "effect result",
            "[changed presentation]",
            gents_protocol::output::PayloadPresentation::Composed {
                parts: vec![gents_protocol::output::PresentationPart::Literal {
                    text: "[changed presentation]".into()
                }]
            },
        )
        .await
        .is_err());
    parent.complete("parent result").await.unwrap();
    let presentation = load_tool_call_presentation(
        &access,
        &child_doc,
        &node_did,
        &session_id,
        requester.as_deref(),
    )
    .await
    .unwrap();
    assert_eq!(presentation.arguments, arguments);
    assert_eq!(presentation.result.as_deref(), Some("[bounded effect]"));
    let read = child
        .access()
        .unwrap()
        .transact_write("test.plugin_child_read", |txn| {
            Box::pin(load_tool_call_read_in_txn(
                txn,
                &child_doc,
                &node_did,
                &session_id,
                requester.as_deref(),
            ))
        })
        .await
        .unwrap();
    assert_eq!(read.raw_result.as_deref(), Some("effect result"));
    assert_eq!(
        render_tool_result(read.result.as_ref().unwrap()).unwrap(),
        "[bounded effect]"
    );
    let headers = scoped_canonical_headers(
        ReadSource::Access(&access),
        &node_did,
        &session_id,
        requester.as_deref(),
    )
    .await
    .unwrap();
    assert!(headers.iter().all(|row| !matches!(&row.message.publication,
            MessagePublication::ToolDelivery { tool_call_doc_id } if tool_call_doc_id == &child_doc)));
    let foreign = PresentedPayload {
        output: child.arguments.clone().unwrap(),
        presentation: gents_protocol::output::PayloadPresentation::Full,
    };
    child.access().unwrap().transact_write("test.forged_child_output_reference", |txn| {
            let foreign = foreign.clone();
            let doc = escape_graphql_string(&child_doc);
            Box::pin(async move {
                txn.execute_with_variables(&format!(
                    r#"mutation($output: JSON) {{ update_AgentToolCall(docID: "{doc}", input: {{ terminal_output: $output }}) {{ _docID }} }}"#
                ), &serde_json::json!({"output": foreign})).await?;
                Ok::<_, anyhow::Error>(())
            })
        }).await.unwrap();
    assert!(load_tool_call_result(
        &access,
        &child_doc,
        &node_did,
        &session_id,
        requester.as_deref()
    )
    .await
    .is_err());
    drop(request_owner);
    node.shutdown().await.expect("shut down test node");
    let _ = std::fs::remove_dir_all(path);
}

#[tokio::test]
async fn plugin_child_timeout_preserves_diagnostic_presentation_and_raw_output() {
    let (fixture, request_owner) =
        gents_toolcalls::call::lifecycle::admission_fixture::published_admission_with_owner(
            gents_toolcalls::call::lifecycle::admission_fixture::PublishedAdmissionOptions {
                name: "plugin-child-timeout-reader".to_owned(),
                composition: gents_automation::lifecycle_hooks::composition(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let gents_toolcalls::call::lifecycle::admission_fixture::PublishedAdmission {
        node,
        path,
        tool: parent,
        ..
    } = fixture;
    let mut child = parent
        .admit_plugin_effect(1, "fixture_effect", "{}")
        .await
        .unwrap();
    child.start_running().await.unwrap();
    let binding = child.tool_output_binding().unwrap();
    gents_toolcalls::call::lifecycle::delivery::append_tool_output(&binding, "partial output")
        .await
        .unwrap();
    child.timeout().await.unwrap();
    let child_doc = child.doc_id().unwrap().to_owned();
    let node_did = child.node_did().to_owned();
    let session_id = child.session_id().to_owned();
    let requester = child.requester_did().map(str::to_owned);
    let read = child
        .access()
        .unwrap()
        .transact_write("test.plugin_child_timeout", |txn| {
            Box::pin(load_tool_call_read_in_txn(
                txn,
                &child_doc,
                &node_did,
                &session_id,
                requester.as_deref(),
            ))
        })
        .await
        .unwrap();
    assert_eq!(read.raw_result.as_deref(), Some("partial output"));
    let presented = render_tool_result(read.result.as_ref().unwrap()).unwrap();
    assert!(presented.contains("partial output"));
    assert!(presented.contains("tool call deadline exceeded"));
    drop(request_owner);
    node.shutdown().await.expect("shut down test node");
    let _ = std::fs::remove_dir_all(path);
}

#[tokio::test]
async fn plugin_child_reader_rejects_foreign_stable_identity_before_loading_arguments() {
    let (fixture, request_owner) =
        gents_toolcalls::call::lifecycle::admission_fixture::published_admission_with_owner(
            gents_toolcalls::call::lifecycle::admission_fixture::PublishedAdmissionOptions {
                name: "plugin-child-identity-reader".to_owned(),
                composition: gents_automation::lifecycle_hooks::composition(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let gents_toolcalls::call::lifecycle::admission_fixture::PublishedAdmission {
        node,
        path,
        tool: parent,
        ..
    } = fixture;
    let parent_doc = escape_graphql_string(parent.doc_id().unwrap());
    let request_doc = escape_graphql_string(parent.request_doc_id().unwrap());
    let session_id = parent.session_id().to_owned();
    let node_did = parent.node_did().to_owned();
    let access = ConfigAccess::local(node.clone());
    let created = access.write("test.forged_plugin_child", &format!(
            r#"mutation {{ create_AgentToolCall(input: {{
                tool_call_key: "forged-effect", tool_call_id: "forged-effect", tool_name: "fixture_effect",
                request_doc_id: "{request_doc}", session_id: "{}", node_did: "{}",
                message_sequence: {}, lifecycle_state: "pending", plugin_parent_tool_call_doc_id: "{parent_doc}",
                plugin_effect_ordinal: 1, await_mode: "foreground", deadline_at: "{}"
            }}) {{ _docID }} }}"#,
            escape_graphql_string(&session_id), escape_graphql_string(&node_did), parent.message_sequence,
            escape_graphql_string(&parent.deadline_at.to_rfc3339()),
        )).await.unwrap();
    let doc_id = gents_store::graphql::created_doc_id(&created, "AgentToolCall").unwrap();
    let error = load_tool_call_arguments(&access, &doc_id, &node_did, &session_id, None)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("invalid plugin effect provenance"),
        "{error:#}"
    );
    drop(request_owner);
    node.shutdown().await.expect("shut down test node");
    let _ = std::fs::remove_dir_all(path);
}

#[tokio::test]
async fn transaction_reader_preserves_raw_beyond_bounded_reply_and_rejects_ambiguity() {
    use gents_protocol::output::{PayloadPresentation, PresentationPart};
    use gents_toolcalls::call::lifecycle::admission_fixture::published_spawn_parent;

    let (node, path, mut tool) = published_spawn_parent("txn-result-reader").await;
    let raw = r#"{"ok":false,"error":{"reason":"wait_timeout"}}"#;
    let bounded = "[bounded presentation]";
    tool.complete_raw_with_presentation(
        raw,
        bounded,
        PayloadPresentation::Composed {
            parts: vec![PresentationPart::Literal {
                text: bounded.to_owned(),
            }],
        },
    )
    .await
    .unwrap();
    let doc_id = tool.doc_id().unwrap().to_owned();
    let node_did = tool.node_did().to_owned();
    let session_id = tool.session_id().to_owned();
    let requester = tool.requester_did().map(str::to_owned);
    let request_doc_id = tool.request_doc_id().unwrap().to_owned();
    let read = tool
        .access()
        .unwrap()
        .transact_write("tool_result.txn_read", |txn| {
            Box::pin(load_tool_call_read_in_txn(
                txn,
                &doc_id,
                &node_did,
                &session_id,
                requester.as_deref(),
            ))
        })
        .await
        .unwrap();
    assert_eq!(read.request_doc_id, request_doc_id);
    assert_eq!(
        read.tool_name,
        gents_protocol::tool_names::SPAWN_PROCESS_TOOL_NAME
    );
    assert_eq!(read.lifecycle_state, ToolCallState::Completed);
    assert!(!read.arguments.is_empty());
    assert_eq!(
        render_tool_result(read.result.as_ref().expect("canonical reply")).unwrap(),
        bounded
    );
    assert_eq!(read.raw_result.as_deref(), Some(raw));

    tool.access()
        .unwrap()
        .transact_write("tool_result.ambiguous_reply", |txn| {
            Box::pin(async {
                let headers = scoped_canonical_headers(
                    ReadSource::Txn(txn),
                    &node_did,
                    &session_id,
                    requester.as_deref(),
                )
                .await?;
                let mut duplicate = headers
                    .iter()
                    .find(|row| is_invocation_reply(&row.message, &doc_id))
                    .expect("fixture completed invocation reply")
                    .message
                    .clone();
                duplicate.message_key.push_str(":duplicate");
                duplicate.sequence += 1;
                txn.execute_with_variables(
                    gents_protocol::output::rows::CREATE_AGENT_MESSAGE_MUTATION,
                    &gents_protocol::output::rows::transcript_message_create_variables(&duplicate)?,
                )
                .await?;
                let error = load_tool_call_read_in_txn(
                    txn,
                    &doc_id,
                    &node_did,
                    &session_id,
                    requester.as_deref(),
                )
                .await
                .unwrap_err();
                assert!(error.to_string().contains("ambiguous invocation reply"));
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();
    node.shutdown().await.expect("shut down test node");
    std::fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn transaction_reader_rejects_tool_delivery_without_native_reply() {
    use gents_toolcalls::call::lifecycle::admission_fixture::published_spawn_parent;

    let (node, path, tool) = published_spawn_parent("txn-malformed-delivery").await;
    let doc_id = tool.doc_id().unwrap().to_owned();
    let node_did = tool.node_did().to_owned();
    let session_id = tool.session_id().to_owned();
    let requester = tool.requester_did().map(str::to_owned);
    tool.access()
        .unwrap()
        .transact_write("tool_result.malformed_reply", |txn| {
            Box::pin(async {
                let headers = scoped_canonical_headers(
                    ReadSource::Txn(txn),
                    &node_did,
                    &session_id,
                    requester.as_deref(),
                )
                .await?;
                let mut malformed = headers
                    .iter()
                    .find(|row| row.message.role == MessageRole::Assistant)
                    .expect("fixture accepted assistant header")
                    .message
                    .clone();
                malformed.message_key.push_str(":malformed-delivery");
                malformed.sequence += 1;
                malformed.publication = MessagePublication::ToolDelivery {
                    tool_call_doc_id: doc_id.clone(),
                };
                txn.execute_with_variables(
                    gents_protocol::output::rows::CREATE_AGENT_MESSAGE_MUTATION,
                    &gents_protocol::output::rows::transcript_message_create_variables(&malformed)?,
                )
                .await?;
                let error = load_tool_call_read_in_txn(
                    txn,
                    &doc_id,
                    &node_did,
                    &session_id,
                    requester.as_deref(),
                )
                .await
                .unwrap_err();
                assert!(error.to_string().contains("carries no native tool-result"));
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();
    node.shutdown().await.expect("shut down test node");
    std::fs::remove_dir_all(path).unwrap();
}
