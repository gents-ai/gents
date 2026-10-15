use super::*;
use crate::config::{BootstrapPolicy, BridgeConfig, HomePolicy, NodeHomePolicy};
use crate::state::resolve_policy;
use serde_json::{json, Value};
use tauri::{ipc::InvokeBody, webview::InvokeRequest, Manager};

fn invoke(
    window: &WebviewWindow<tauri::test::MockRuntime>,
    command: &str,
    arguments: Value,
) -> Result<Value, Value> {
    tauri::test::get_ipc_response(
        window,
        InvokeRequest {
            cmd: command.into(),
            callback: tauri::ipc::CallbackFn(0),
            error: tauri::ipc::CallbackFn(1),
            url: "http://tauri.localhost".parse().unwrap(),
            body: InvokeBody::Json(arguments),
            headers: Default::default(),
            invoke_key: tauri::test::INVOKE_KEY.into(),
        },
    )
    .map(|response| response.deserialize().unwrap())
}

#[test]
fn prompt_commands_are_scoped_to_live_native_windows_and_mount_tokens() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("node");
    let config = BridgeConfig {
        home: HomePolicy::FixedRoot(temp.path().join("desktop")),
        bootstrap: BootstrapPolicy::LocalRuntimeAllowed {
            node_home: NodeHomePolicy::Fixed(home.clone()),
        },
        ..Default::default()
    };
    let app = tauri::test::mock_builder()
        .manage(DesktopAppState::new(resolve_policy(&config, None).unwrap()))
        .invoke_handler(tauri::generate_handler![
            desktop_plugin_approval_subscribe,
            desktop_plugin_approval_unsubscribe,
            desktop_plugin_approvals_pending
        ])
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let main = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();
    let second = tauri::WebviewWindowBuilder::new(&app, "second", Default::default())
        .build()
        .unwrap();
    let state = app.state::<DesktopAppState>();
    state.register_window("main");
    state.register_window("second");
    let active = state
        .bridge
        .lock()
        .unwrap()
        .approvals_prompt_subscriptions
        .changes();
    let mark = home
        .join(gents::home::PLUGIN_APPROVALS_DIR_NAME)
        .join(".listening");

    assert_eq!(
        invoke(&main, "desktop_plugin_approvals_pending", json!({})).unwrap(),
        json!({"requests": []})
    );
    assert!(!mark.exists());
    for token in ["old", "new"] {
        invoke(
            &main,
            "desktop_plugin_approval_subscribe",
            json!({"token": token}),
        )
        .unwrap();
    }
    invoke(
        &main,
        "desktop_plugin_approval_unsubscribe",
        json!({"token": "old"}),
    )
    .unwrap();
    invoke(
        &second,
        "desktop_plugin_approval_unsubscribe",
        json!({"token": "new"}),
    )
    .unwrap();
    assert!(*active.borrow());
    invoke(&main, "desktop_plugin_approvals_pending", json!({})).unwrap();
    assert!(
        !mark.exists(),
        "a read cannot advertise before native watch attachment"
    );
    main.hide().unwrap();
    state.window_event("main", &tauri::WindowEvent::Focused(false));
    assert!(*active.borrow());
    state.window_event("main", &tauri::WindowEvent::Destroyed);
    assert!(!*active.borrow());
    invoke(
        &main,
        "desktop_plugin_approval_subscribe",
        json!({"token": "late"}),
    )
    .unwrap();
    assert!(
        !*active.borrow(),
        "late IPC cannot recreate a destroyed prompt owner"
    );
}

#[test]
fn a_failed_subscription_does_not_advertise_a_listener() {
    let temp = tempfile::tempdir().unwrap();
    let config = BridgeConfig {
        home: HomePolicy::FixedRoot(temp.path().join("desktop")),
        bootstrap: BootstrapPolicy::PairedRemoteOnly,
        ..Default::default()
    };
    let app = tauri::test::mock_builder()
        .manage(DesktopAppState::new(resolve_policy(&config, None).unwrap()))
        .invoke_handler(tauri::generate_handler![desktop_plugin_approval_subscribe])
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let window = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();
    let state = app.state::<DesktopAppState>();
    state.register_window("main");
    let active = state
        .bridge
        .lock()
        .unwrap()
        .approvals_prompt_subscriptions
        .changes();
    assert!(invoke(
        &window,
        "desktop_plugin_approval_subscribe",
        json!({"token": "failed"})
    )
    .is_err());
    assert!(!*active.borrow());
}
