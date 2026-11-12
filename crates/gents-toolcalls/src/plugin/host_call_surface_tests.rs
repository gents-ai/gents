use gents_tools::surface::{AgentToolSurfaceConfig, ToolCeiling};

fn tools_document(mut value: serde_json::Value) -> gents_documents::Tools {
    let fields = value.as_object_mut().unwrap();
    fields
        .entry("tools_id")
        .or_insert(serde_json::json!("test-tools"));
    fields
        .entry("node_did")
        .or_insert(serde_json::json!("did:test:test"));
    serde_json::from_value(value).unwrap()
}

#[tokio::test]
async fn only_a_host_call_plugin_adds_the_process_tools() {
    let node_db = gents_inference::test_support::test_db(
        "only-a-host-call-plugin-adds-the-process",
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = node_db.node.clone();
    let node_did = node.node_identity_did().unwrap().to_owned();
    let without = tools_document(serde_json::json!({}));
    let with_plugin = tools_document(serde_json::json!({
        "integrations": {"plugins": [{"plugin": "team/plugin"}]}
    }));
    for host_call in [false, true] {
        let (home, mut record) = gents_pack::test_support::installed_echo();
        if host_call {
            record.declaration.host_imports = vec![gents_pack::pack::HostImport::HostCall];
            gents_pack::plugin::store::write_record(home.path(), &record).unwrap();
        }
        let plugins = crate::plugin::executor::PluginExecutor::new(Some(home.path().to_owned()));
        let mut names = Vec::new();
        let mut background = Vec::new();
        for document in [&without, &with_plugin] {
            let surface = AgentToolSurfaceConfig::from_tools_document(
                "scripted",
                document,
                &ToolCeiling::meta_only(),
                Vec::new(),
            )
            .unwrap()
            .resolve(&node, &node_did, &plugins)
            .await
            .unwrap();
            names.push(surface.tool_names());
            background.push(surface.background_tools().allowlist.clone());
        }
        let mut expected = names[0].clone();
        expected.push("plugin".to_owned());
        let process_tools = gents_tools::native::background_tool_names(
            &gents_tools::surface::BackgroundToolConfig {
                allowlist: vec!["plugin".to_owned()],
                timeouts: Default::default(),
            },
        );
        if host_call {
            expected.extend(process_tools);
            assert_eq!(background[1], ["plugin".to_owned()]);
        } else {
            assert!(background[1].is_empty(), "{:?}", background[1]);
        }
        assert!(background[0].is_empty());
        let sorted = |mut names: Vec<String>| {
            names.sort();
            names
        };
        assert_eq!(
            sorted(names[1].clone()),
            sorted(expected),
            "host_call {host_call}"
        );
    }
}
