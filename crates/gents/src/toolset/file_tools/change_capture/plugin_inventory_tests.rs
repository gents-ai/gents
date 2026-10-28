use super::*;
use crate::config_client::ConfigAccess;
use crate::llm::tool::Tool;
use crate::tool_call_lifecycle::admission_fixture::{
    published_admission_with_owner, PublishedAdmissionOptions,
};
use crate::toolset::args::WriteFileArgs;
use crate::toolset::file_tools::WriteFileTool;
use crate::toolset::shared::ToolContext;
use gents_loop::live_output::LiveToolOutputRegistry;

#[tokio::test]
async fn plugin_file_effects_retain_exact_images_without_borrowing_parent_order() {
    let contracts: serde_json::Value = tokio::task::spawn_blocking(
        gents_lean_contract::load_contract_snapshot::<serde_json::Value>,
    )
    .await
    .unwrap()
    .unwrap();
    let modeled = contracts["file_inventory_order_window_cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "accepted_effect_without_own_publication")
        .unwrap();
    assert!(modeled["expected"].is_null());
    let (fixture, _request) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "inventory-plugin-effects".into(),
        real_identity: true,
        paired_client: true,
        tool_name: Some("plugin".into()),
        ..Default::default()
    })
    .await
    .unwrap();
    let mut parent = fixture.tool;
    let root = tempfile::tempdir().unwrap();
    let root_path = std::fs::canonicalize(root.path()).unwrap();
    let path = root_path.join("file.txt");
    tokio::fs::write(&path, "before\n").await.unwrap();
    let writer_tool = WriteFileTool::new(ToolContext::new(root_path.clone(), false).unwrap());
    let registry = LiveToolOutputRegistry::default();
    let access = ConfigAccess::local(fixture.node.clone());
    let mut captures = Vec::new();
    let mut tools = Vec::new();
    for (ordinal, before, after) in [
        (1, "before\n", "first effect\n"),
        (2, "first effect\n", "second effect\n"),
    ] {
        let arguments = serde_json::json!({"path": "file.txt", "content": after,
            "overwrite": true, "raw_json": true})
        .to_string();
        let mut effect = parent
            .admit_plugin_effect(ordinal, "write_file", &arguments)
            .await
            .unwrap();
        effect.start_running().await.unwrap();
        let document = effect.doc_id().unwrap().to_owned();
        let writer = registry.writer_for(document.clone()).await;
        let output = gents_loop::tool_call_lifecycle::runtime::scope_request_tool_execution_with_trigger_context(
            None, tokio_util::sync::CancellationToken::new(), None, Some(writer), None, None,
            Default::default(), false,
            writer_tool.call(WriteFileArgs {path: "file.txt".into(), content: after.into(),
                expected_content_hash: None, overwrite: true, raw_json: true}),
        ).await.unwrap();
        effect.stage_deferred_writes(registry.deferred_writes(&document).await);
        effect.complete(&output).await.unwrap();
        let replayed = parent
            .admit_plugin_effect(ordinal, "write_file", &arguments)
            .await
            .unwrap();
        assert_eq!(replayed.doc_id(), Some(document.as_str()));
        let capture = crate::tool_call_lifecycle::query::load_tool_call_file_changes(
            &access,
            &document,
            effect.request_doc_id().unwrap(),
            effect.node_did(),
            effect.session_id(),
            effect.requester_did(),
        )
        .await
        .unwrap()
        .expect("terminal effect retains its native file observation");
        assert_eq!(capture.changes.len(), 1);
        let change = &capture.changes[0];
        assert_eq!(change.root, root_path.to_str().unwrap());
        assert_eq!(change.path, "file.txt");
        for (image, expected) in [(&change.before, before), (&change.after, after)] {
            let FileImage::File {
                data,
                content_hash: hash,
                ..
            } = image
            else {
                panic!("expected retained file bytes");
            };
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data)
                .unwrap();
            assert_eq!(bytes, expected.as_bytes());
            assert_eq!(hash, &content_hash(&bytes));
        }
        captures.push(capture);
        tools.push(document);
    }
    assert_eq!(captures[0].changes[0].after, captures[1].changes[0].before);
    parent.complete("both effects completed").await.unwrap();
    let admission = ConfigAccess::transact_local_readonly(
        &fixture.node,
        None,
        "test.inventory.plugin_session",
        |txn| {
            let parent = &parent;
            Box::pin(async move {
                let session = crate::session::load_agent_session_row_in_txn(
                    txn,
                    parent.node_did(),
                    parent.session_id(),
                    parent.requester_did(),
                )
                .await?
                .unwrap();
                Ok(gents_protocol::file_inventory::FileInventoryAdmission {
                    session_doc_id: session.doc_id,
                    request: gents_protocol::session_hydration::HydrationRequest {
                        request_key: "plugin-inventory".into(),
                        peer_id: "fixture-peer".into(),
                        node_did: parent.node_did().into(),
                        session_id: parent.session_id().into(),
                        requester_did: parent.requester_did().unwrap().into(),
                    },
                })
            })
        },
    )
    .await
    .unwrap();
    let snapshot = crate::file_inventory::FileInventorySnapshot::load(&fixture.node, &admission)
        .await
        .unwrap();
    assert_eq!(
        snapshot.entries().len(),
        2,
        "replay cannot duplicate an effect capture"
    );
    for entry in snapshot.entries() {
        assert!(tools.contains(&entry.tool_call_doc_id));
        assert_eq!(entry.request_doc_id, parent.request_doc_id().unwrap());
        assert_eq!(entry.scope.node_did, parent.node_did());
        assert_eq!(entry.scope.session_id, parent.session_id());
        assert_eq!(entry.scope.requester_did, parent.requester_did().unwrap());
        assert_eq!(
            serde_json::to_value(&entry.order).unwrap(),
            modeled["expected"],
            "continuous images and effect ordinals do not supply a canonical publication pair"
        );
    }
    let page = snapshot.page(None).unwrap();
    assert_eq!(page.entries, snapshot.entries());
    assert!(page.next.is_none());
    let repeated = crate::file_inventory::FileInventorySnapshot::load(&fixture.node, &admission)
        .await
        .unwrap();
    assert_eq!(snapshot.binding(), repeated.binding());
    assert_eq!(snapshot.entries(), repeated.entries());
    assert_eq!(tokio::fs::read(&path).await.unwrap(), b"second effect\n");
    fixture.node.shutdown().await;
    tokio::fs::remove_dir_all(fixture.path).await.unwrap();
}
