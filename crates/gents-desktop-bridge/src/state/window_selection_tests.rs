use super::*;

fn state() -> (tempfile::TempDir, DesktopAppState) {
    let temp = tempfile::tempdir().expect("tempdir");
    let config = BridgeConfig {
        home: HomePolicy::FixedRoot(temp.path().join("desktop")),
        bootstrap: BootstrapPolicy::PairedRemoteOnly,
        store_key_custody: gents::store_key::StoreKeyCustodyChoice::File,
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
        .set_window_selected_node(label, did.map(str::to_owned));
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
