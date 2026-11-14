use gents_session::tool_call::query::result::*;

#[tokio::test]
async fn generated_plugin_child_terminal_presentation_matches_execution_owner() {
    use gents_protocol::output::{PayloadPresentation, PresentationPart};
    let cases = gents_lean_vocab::families::plugin_effects_terminal_presentation::cases();
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
        let accepted = if case.input.replay {
            let conflict = case.input.conflict;
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
        case.check(accepted);
        drop(request_owner);
        node.shutdown().await.expect("shut down test node");
        let _ = std::fs::remove_dir_all(path);
    }
}
