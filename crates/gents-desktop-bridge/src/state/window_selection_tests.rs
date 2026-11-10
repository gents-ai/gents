use super::*;

fn state() -> (tempfile::TempDir, DesktopAppState) {
    let temp = tempfile::tempdir().expect("tempdir");
    let config = BridgeConfig {
        home: HomePolicy::FixedRoot(temp.path().join("desktop")),
        bootstrap: BootstrapPolicy::PairedRemoteOnly,
        store_key_custody: gents::store::local::store_key::StoreKeyCustodyChoice::File,
        ..Default::default()
    };
    let policy = resolve_policy(&config, None).expect("policy");
    (temp, DesktopAppState::new(policy))
}

fn select(state: &DesktopAppState, label: &str, did: Option<&str>) {
    state
        .bridge
        .lock()
        .unwrap()
        .select_window_node(label, did.map(str::to_owned), |_| {});
}

fn selected(state: &DesktopAppState) -> Option<String> {
    state.bridge.lock().unwrap().selected_node_did()
}

#[test]
fn closing_either_window_restores_the_survivors_latest_selection_without_focus() {
    for (closed, survivor, expected) in [
        ("main", "second", "did:second"),
        ("second", "main", "did:updated"),
    ] {
        let (_temp, state) = state();
        state.register_window("main");
        select(&state, "main", Some("did:first"));
        assert_eq!(selected(&state).as_deref(), Some("did:first"));

        state.register_window("second");
        assert_eq!(selected(&state), None);
        select(&state, "second", Some("did:second"));
        select(&state, "main", Some("did:updated"));
        assert_eq!(selected(&state), None);

        state.window_event(closed, &tauri::WindowEvent::Focused(false));
        assert_eq!(selected(&state), None);
        state.window_event(closed, &tauri::WindowEvent::Destroyed);
        assert_eq!(selected(&state).as_deref(), Some(expected));
        select(&state, closed, Some("did:stale"));
        state.window_event(closed, &tauri::WindowEvent::Destroyed);
        assert_eq!(selected(&state).as_deref(), Some(expected));

        state.window_event(survivor, &tauri::WindowEvent::Destroyed);
        assert_eq!(selected(&state), None);
        select(&state, survivor, Some("did:stale"));
        assert!(!state.bridge.lock().unwrap().has_window(survivor));
    }
}

#[test]
fn an_unselected_survivor_and_multiple_views_keep_the_scope_open() {
    let (_temp, state) = state();
    state.register_window("main");
    state.register_window("second");
    state.register_window("third");
    select(&state, "main", Some("did:first"));
    select(&state, "second", Some("did:second"));
    state.window_event("main", &tauri::WindowEvent::Destroyed);
    assert_eq!(selected(&state), None);
    state.window_event("second", &tauri::WindowEvent::Destroyed);
    assert_eq!(selected(&state), None);
    select(&state, "third", Some("did:third"));
    state.register_window("third");
    assert_eq!(selected(&state).as_deref(), Some("did:third"));
    select(&state, "third", None);
    assert_eq!(selected(&state), None);
}

#[tokio::test]
async fn window_scope_reaches_the_core_and_survives_core_recreation() {
    let (_temp, state) = state();
    state.register_window("main");
    select(&state, "main", Some("did:first"));
    let core = Arc::new(
        ClientCore::start_with_paths_and_options(
            state.policy.desktop_paths.clone(),
            gents_desktop_core::client::ClientCoreOptions::local_only(),
        )
        .await
        .expect("client core"),
    );
    state.bridge.lock().unwrap().install_core(Arc::clone(&core));
    assert_eq!(core.selected_node_did().as_deref(), Some("did:first"));
    let mut scope = core.selected_node_did_rx();

    state.register_window("second");
    assert!(scope.has_changed().unwrap());
    assert_eq!(*scope.borrow_and_update(), None);
    select(&state, "second", Some("did:second"));
    select(&state, "main", Some("did:updated"));
    assert!(!scope.has_changed().unwrap());
    state.window_event("second", &tauri::WindowEvent::Destroyed);
    assert_eq!(core.selected_node_did().as_deref(), Some("did:updated"));
    scope.borrow_and_update();
    select(&state, "second", Some("did:stale"));
    assert!(!scope.has_changed().unwrap());

    state.bridge.lock().unwrap().core.take();
    core.shutdown().await.expect("shutdown first core");
    drop(core);
    let core = Arc::new(
        ClientCore::start_with_paths_and_options(
            state.policy.desktop_paths.clone(),
            gents_desktop_core::client::ClientCoreOptions::local_only(),
        )
        .await
        .expect("recreated client core"),
    );
    state.bridge.lock().unwrap().install_core(Arc::clone(&core));
    assert_eq!(core.selected_node_did().as_deref(), Some("did:updated"));
    state.window_event("main", &tauri::WindowEvent::Destroyed);
    assert_eq!(core.selected_node_did(), None);
    state.bridge.lock().unwrap().core.take();
    core.shutdown().await.expect("shutdown recreated core");
}

/// Regression: the selection was written and the watches cancelled after it
/// in a second critical section, so a delivery between them sent the old
/// node's snapshot to a window that selects the new one (TLA
/// `MCPackUxWatchSelectBeforeCancel`).
#[test]
fn a_selection_change_cancels_watches_while_the_old_node_is_selected() {
    let (_temp, state) = state();
    state.register_window("main");
    select(&state, "main", Some("did:a"));
    let watch = state.ux_watches.insert("main", "watch").unwrap();
    let mut bridge = state.bridge.lock().unwrap();
    let mut old_node_selected = None;
    assert!(
        bridge.select_window_node("main", Some("did:b".into()), |bridge| {
            old_node_selected = Some(bridge.window_selects_node("main", "did:a"));
            state.ux_watches.cancel_window("main");
        })
    );
    assert_eq!(old_node_selected, Some(true));
    assert!(watch.is_cancelled());
    assert!(!state.ux_watches.deliver("main", "watch", || true));
    assert!(bridge.window_selects_node("main", "did:b"));

    let kept = state.ux_watches.insert("main", "kept").unwrap();
    assert!(
        !bridge.select_window_node("main", Some("did:b".into()), |_| {
            state.ux_watches.cancel_window("main");
        })
    );
    assert!(!kept.is_cancelled());
}

#[test]
fn destroying_a_window_cancels_only_its_pack_observations() {
    let (_temp, state) = state();
    state.register_window("main");
    state.register_window("second");
    let main = state.ux_watches.insert("main", "same").unwrap();
    let other = state.ux_watches.insert("second", "same").unwrap();
    state.window_event("main", &tauri::WindowEvent::Destroyed);
    assert!(main.is_cancelled());
    assert!(!other.is_cancelled());
}

#[test]
fn destroying_or_navigating_a_window_retires_only_its_prompt_subscriptions() {
    let (_temp, state) = state();
    state.register_window("main");
    state.register_window("second");
    let active = {
        let mut bridge = state.bridge.lock().unwrap();
        bridge
            .approvals_prompt_subscriptions
            .subscribe("main", "main-prompt".into());
        bridge
            .approvals_prompt_subscriptions
            .subscribe("second", "second-prompt".into());
        bridge.approvals_prompt_subscriptions.changes()
    };
    state.window_event("main", &tauri::WindowEvent::Focused(false));
    assert!(state
        .bridge
        .lock()
        .unwrap()
        .approvals_prompt_subscriptions
        .contains_window("main"));
    state.window_event("main", &tauri::WindowEvent::Destroyed);
    assert!(*active.borrow());
    assert!(!state
        .bridge
        .lock()
        .unwrap()
        .approvals_prompt_subscriptions
        .contains_window("main"));
    state.clear_plugin_approval_subscriptions("second");
    assert!(!*active.borrow());
    assert!(state.bridge.lock().unwrap().has_window("second"));
}

#[tokio::test]
async fn external_attachment_release_retains_the_current_window_scope() {
    let contract: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let actions = serde_json::json!([
        {"kind":"select", "node":null}, {"kind":"detach"}, {"kind":"select", "node":1}
    ]);
    let trace = contract["client_observation_scope_cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| {
            case["initial"]["requested"] == 1
                && case["initial"]["attachments"] == 1
                && case["actions"] == actions
        })
        .expect("modeled desktop-window and external-attachment trace");
    let expected = |index: usize| {
        trace["expected"][index]["effective"]
            .as_u64()
            .map(|id| format!("did:scope:{id}"))
    };
    let (_temp, state) = state();
    state.register_window("main");
    select(&state, "main", Some("did:scope:1"));
    let core = Arc::new(
        ClientCore::start_with_paths_and_options(
            state.policy.desktop_paths.clone(),
            gents_desktop_core::client::ClientCoreOptions::local_only(),
        )
        .await
        .unwrap(),
    );
    state.bridge.lock().unwrap().install_core(core.clone());
    let attachment = core.observe_all_nodes().unwrap();
    assert_eq!(core.selected_node_did(), expected(0));
    state.register_window("second");
    select(&state, "second", Some("did:scope:2"));
    assert_eq!(core.selected_node_did(), expected(1));
    drop(attachment);
    assert_eq!(core.selected_node_did(), expected(2));
    state.window_event("second", &tauri::WindowEvent::Destroyed);
    assert_eq!(core.selected_node_did(), expected(3));
    state.bridge.lock().unwrap().core.take();
    core.shutdown().await.unwrap();
}

/// Replays window traces through the bridge's window registry and checks
/// every window's command node and deliveries after each step against the
/// executed Lean `ClientWindowScope` owner.
#[test]
fn generated_window_scope_traces_match_the_lean_owner() {
    const WINDOWS: [&str; 3] = ["main", "second", "third"];
    const NODES: [&str; 2] = ["did:node:0", "did:node:1"];
    let op = |op: &str, window: usize, node: Option<usize>| serde_json::json!({"op": op, "window": window, "node": node});
    let mut alphabet = Vec::new();
    for window in 0..WINDOWS.len() {
        alphabet.push(op("register", window, None));
        alphabet.push(op("destroy", window, None));
        alphabet.push(op("select", window, None));
        for node in 0..NODES.len() {
            alphabet.push(op("select", window, Some(node)));
        }
    }
    let prefix = [op("register", 0, None), op("register", 1, None)];
    let mut traces = Vec::new();
    for a in &alphabet {
        for b in &alphabet {
            for c in &alphabet {
                let ops: Vec<_> = prefix.iter().chain([a, b, c]).cloned().collect();
                traces.push(
                    serde_json::json!({"windows": WINDOWS.len(), "nodes": NODES.len(), "ops": ops}),
                );
            }
        }
    }
    let output = gents_lean_contract::run_prepared_generator_with_input(
        &gents_lean_contract::proofs_dir().unwrap(),
        "Proofs.Conformance.ClientWindowScope:olean",
        "Proofs/Conformance/ClientWindowScope.lean",
        &serde_json::to_string(&traces).unwrap(),
    )
    .unwrap();
    let modeled: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
    assert_eq!(modeled.len(), traces.len());
    let node_index =
        |did: Option<String>| did.map(|did| NODES.iter().position(|node| *node == did).unwrap());

    let (_temp, state) = state();
    let mut checked = 0;
    for (trace, steps) in traces.iter().zip(&modeled) {
        for label in WINDOWS {
            state.window_event(label, &tauri::WindowEvent::Destroyed);
        }
        let ops = trace["ops"].as_array().unwrap();
        let steps = steps.as_array().unwrap();
        assert_eq!(ops.len(), steps.len());
        for (step, windows) in ops.iter().zip(steps) {
            let label = WINDOWS[usize::try_from(step["window"].as_u64().unwrap()).unwrap()];
            let node = step["node"]
                .as_u64()
                .map(|node| NODES[usize::try_from(node).unwrap()].to_owned());
            match step["op"].as_str().unwrap() {
                "register" => state.register_window(label),
                "destroy" => state.window_event(label, &tauri::WindowEvent::Destroyed),
                "select" => select(&state, label, node.as_deref()),
                other => panic!("unknown window op {other}"),
            }
            for (window, expected) in windows.as_array().unwrap().iter().enumerate() {
                let label = WINDOWS[window];
                let bridge = state.bridge.lock().unwrap();
                let observed = serde_json::json!({
                    "command": node_index(bridge.window_command_node(label, None)),
                    "named": NODES
                        .iter()
                        .map(|node| node_index(bridge.window_command_node(label, Some(node))))
                        .collect::<Vec<_>>(),
                    "delivers": (0..NODES.len())
                        .filter(|node| bridge.window_selects_node(label, NODES[*node]))
                        .collect::<Vec<_>>(),
                });
                assert_eq!(&observed, expected, "{label} after {step} in {trace}");
                checked += 1;
            }
        }
    }
    assert!(checked > 10_000, "{checked}");
}

fn invoke(
    window: &tauri::WebviewWindow<tauri::test::MockRuntime>,
    command: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value, String> {
    tauri::test::get_ipc_response(
        window,
        tauri::webview::InvokeRequest {
            cmd: command.into(),
            callback: tauri::ipc::CallbackFn(0),
            error: tauri::ipc::CallbackFn(1),
            url: "http://tauri.localhost".parse().unwrap(),
            body: tauri::ipc::InvokeBody::Json(arguments),
            headers: Default::default(),
            invoke_key: tauri::test::INVOKE_KEY.into(),
        },
    )
    .map(|response| response.deserialize().unwrap())
    .map_err(|error| error.to_string())
}

/// Two windows on two nodes through the production commands: each window's
/// selection is made with `desktop_set_selected_node`, each window's
/// node-scoped command resolves its own node (the commands' error names the
/// node they acted on, since no runtime is routed here), a node switch
/// cancels only the switching window's pack watches, and a destroyed window
/// acts on nothing. Nothing here routes for the commands: the window label
/// Tauri passes is the only input.
#[test]
fn two_windows_on_two_nodes_resolve_through_their_own_selection() {
    // The test entry's runtime drives the client core; the commands run on
    // Tauri's.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (_temp, state) = state();
    let core = Arc::new(
        runtime
            .block_on(ClientCore::start_with_paths_and_options(
                state.policy.desktop_paths.clone(),
                gents_desktop_core::client::ClientCoreOptions::local_only(),
            ))
            .expect("client core"),
    );
    state.bridge.lock().unwrap().install_core(Arc::clone(&core));
    let app = tauri::test::mock_builder()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            crate::tauri_commands::lifecycle::desktop_set_selected_node,
            crate::tauri_commands::operations::desktop_list_backends_with_health,
            crate::tauri_commands::operations::desktop_operations_snapshot,
        ])
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let main = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();
    let second = tauri::WebviewWindowBuilder::new(&app, "second", Default::default())
        .build()
        .unwrap();
    let state = tauri::Manager::state::<DesktopAppState>(&app);
    state.register_window("main");
    state.register_window("second");
    let acted_on = |window: &tauri::WebviewWindow<tauri::test::MockRuntime>| {
        let backends = invoke(
            window,
            "desktop_list_backends_with_health",
            serde_json::json!({}),
        )
        .expect_err("no runtime is routed");
        let operations = invoke(
            window,
            "desktop_operations_snapshot",
            serde_json::json!({"request": {}}),
        )
        .expect_err("no runtime is routed");
        [backends, operations].map(|error| {
            ["did:a", "did:b"]
                .into_iter()
                .find(|node| error.contains(&format!("runtime {node} ")))
        })
    };

    invoke(
        &main,
        "desktop_set_selected_node",
        serde_json::json!({"nodeDid": "did:a"}),
    )
    .unwrap();
    invoke(
        &second,
        "desktop_set_selected_node",
        serde_json::json!({"nodeDid": "did:b"}),
    )
    .unwrap();
    assert_eq!(
        core.selected_node_did(),
        None,
        "no shared selection with two windows"
    );
    assert_eq!(acted_on(&main), [Some("did:a"); 2]);
    assert_eq!(acted_on(&second), [Some("did:b"); 2]);
    let named = invoke(
        &second,
        "desktop_operations_snapshot",
        serde_json::json!({"request": {"nodeDid": "did:a"}}),
    )
    .unwrap_err();
    assert!(
        named.contains("runtime did:a "),
        "a named node wins: {named}"
    );

    // What `desktop_ux_data_watch` registers for each window's own node.
    let main_watch = state.ux_watches.insert("main", "pack-a").unwrap();
    let second_watch = state.ux_watches.insert("second", "pack-b").unwrap();
    invoke(
        &main,
        "desktop_set_selected_node",
        serde_json::json!({"nodeDid": "did:b"}),
    )
    .unwrap();
    assert!(
        main_watch.is_cancelled(),
        "the switching window's watch ends"
    );
    assert!(
        !second_watch.is_cancelled(),
        "the other window keeps its watch"
    );
    assert!(!state.ux_watches.deliver("main", "pack-a", || true));
    assert!(state.ux_watches.deliver("second", "pack-b", || true));
    assert_eq!(acted_on(&main), [Some("did:b"); 2]);
    assert_eq!(acted_on(&second), [Some("did:b"); 2]);

    state.window_event("second", &tauri::WindowEvent::Destroyed);
    assert!(second_watch.is_cancelled());
    invoke(
        &second,
        "desktop_set_selected_node",
        serde_json::json!({"nodeDid": "did:a"}),
    )
    .expect("late IPC from a destroyed window is ignored");
    assert_eq!(
        acted_on(&second),
        [None; 2],
        "a destroyed window acts on nothing"
    );
    assert_eq!(acted_on(&main), [Some("did:b"); 2]);
    assert_eq!(core.selected_node_did().as_deref(), Some("did:b"));

    state.bridge.lock().unwrap().core.take();
    runtime.block_on(core.shutdown()).unwrap();
}
