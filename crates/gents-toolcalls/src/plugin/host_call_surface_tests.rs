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
    let node_db = gents_store::test_support::TestStore::start(
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
                limit: Default::default(),
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

#[tokio::test]
async fn a_plugin_surface_built_during_an_install_waits_for_the_installer() {
    crate::test_support::enable_scoped_event_capture();
    let node_db = gents_store::test_support::TestStore::start(
        "a-plugin-surface-built-during-an-install",
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = node_db.node.clone();
    let node_did = node.node_identity_did().unwrap().to_owned();
    let (home, _) = gents_pack::test_support::installed_echo();
    let plugins = std::sync::Arc::new(crate::plugin::executor::PluginExecutor::new(Some(
        home.path().to_owned(),
    )));
    let surface = AgentToolSurfaceConfig::from_tools_document(
        "scripted",
        &tools_document(serde_json::json!({
            "integrations": {"plugins": [{"plugin": "team/plugin"}]}
        })),
        &ToolCeiling::meta_only(),
        Vec::new(),
    )
    .unwrap()
    .resolve(&node, &node_did, plugins.as_ref())
    .await
    .unwrap();
    let runtime =
        gents_tools::surface::ToolRuntimeContext::oneshot_with_node_did(node.clone(), &node_did)
            .with_agent_for_test(node_db.node_identity.clone(), node_db.node_identity.clone())
            .with_plugins(plugins);
    let installer = gents_pack::plugin::store::lock_store(home.path()).unwrap();
    let deadline = tokio::time::Instant::now() + gents_tools::surface::PUBLICATION_WAIT;
    let (waiting_tx, mut waiting) = tokio::sync::mpsc::unbounded_channel();
    let building = tokio::spawn(tracing::instrument::WithSubscriber::with_subscriber(
        async move {
            surface
                .build_tools_until(&runtime, deadline)
                .await
                .map(|tools| tools.iter().map(|tool| tool.name()).collect::<Vec<_>>())
        },
        tracing_subscriber::layer::SubscriberExt::with(
            tracing_subscriber::Registry::default(),
            PublicationWaits(waiting_tx),
        ),
    ));
    if waiting.recv().await.is_none() {
        panic!(
            "the construction never met the installer's lock: {:?}",
            building.await.unwrap()
        );
    }
    assert!(!building.is_finished());
    drop(installer);
    let names = building.await.unwrap().unwrap();
    assert!(names.iter().any(|name| name == "plugin"), "{names:?}");
}

struct PublicationWaits(tokio::sync::mpsc::UnboundedSender<()>);

impl<S> tracing_subscriber::Layer<S> for PublicationWaits
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if event.metadata().target() == gents_tools::surface::PLUGIN_PUBLICATION_WAIT_EVENT_TARGET {
            let _ = self.0.send(());
        }
    }
}
