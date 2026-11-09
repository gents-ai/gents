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
