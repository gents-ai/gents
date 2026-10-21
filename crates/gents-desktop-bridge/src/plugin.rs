use tauri::plugin::{Builder, TauriPlugin};
use tauri::{Manager, Runtime};

use crate::config::BridgeConfig;
use crate::state::{resolve_policy, DesktopAppState};
use crate::tauri_commands;

pub fn init<R: Runtime>(config: BridgeConfig) -> TauriPlugin<R> {
    Builder::<R>::new("gents-desktop-bridge")
        .setup(move |app, _api| {
            let app_data_dir = app.path().app_data_dir().ok();
            let policy = resolve_policy(&config, app_data_dir)
                .map_err(|error| Box::<dyn std::error::Error>::from(error))?;
            tracing::info!(
                app = %policy.app_meta.app_name,
                version = %policy.app_meta.app_version,
                home = %policy.desktop_paths.root().display(),
                "gents-desktop-bridge initialized"
            );
            hold_desktop_process(app, &policy.desktop_paths)?;
            app.manage(DesktopAppState::new(policy));
            let state = app.state::<DesktopAppState>();
            if let Some(home) = state.policy.node_home.clone() {
                let subscriptions = state
                    .bridge
                    .lock()
                    .expect("desktop bridge lock poisoned")
                    .approvals_prompt_subscriptions
                    .changes();
                let watch = crate::approvals_watch::spawn_plugin_approvals_watch(
                    app.clone(),
                    home,
                    subscriptions,
                );
                state
                    .bridge
                    .lock()
                    .expect("desktop bridge lock poisoned")
                    .approvals_watch_task = Some(watch);
            }
            let handle = app.clone();
            std::thread::spawn(move || {
                let state = handle.state::<DesktopAppState>();
                if let Err(error) =
                    tauri_commands::managed_server::refresh_packaged_install(&handle, &state)
                {
                    tracing::warn!(
                        error = %error.message,
                        "could not bring the packaged Gents runtime up to date"
                    );
                }
                if let Err(error) = tauri::async_runtime::block_on(
                    tauri_commands::managed_server::restart_outdated_managed_job(&handle, &state),
                ) {
                    tracing::warn!(
                        error = %error.message,
                        "could not restart the node service that predates this app"
                    );
                }
                if let Err(error) = tauri::async_runtime::block_on(
                    tauri_commands::managed_server::start_configured_managed_server(
                        &handle, &state,
                    ),
                ) {
                    tracing::warn!(
                        error = %error.message,
                        "could not start the configured local node"
                    );
                }
            });
            Ok(())
        })
        .on_webview_ready(register_window)
        .on_page_load(|webview, payload| {
            if matches!(payload.event(), tauri::webview::PageLoadEvent::Started) {
                webview
                    .state::<DesktopAppState>()
                    .clear_plugin_approval_subscriptions(webview.window().label());
            }
        })
        .on_event(|app, event| {
            if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
                crate::app_exit::requested(app, api, *code);
            }
            if let tauri::RunEvent::WindowEvent { label, event, .. } = event {
                app.state::<DesktopAppState>().window_event(label, event);
            }
        })
        .invoke_handler(tauri::generate_handler![
            tauri_commands::external_url::desktop_open_external_url,
            tauri_commands::packs::desktop_pack_installed,
            tauri_commands::packs::desktop_pack_search,
            tauri_commands::packs::desktop_pack_info,
            tauri_commands::packs::desktop_pack_whoami,
            tauri_commands::packs::desktop_pack_install,
            tauri_commands::packs::desktop_pack_update,
            tauri_commands::packs::desktop_pack_remove,
            tauri_commands::packs::desktop_pack_login,
            tauri_commands::packs::desktop_pack_logout,
            tauri_commands::packs::desktop_pack_plugin_slots,
            tauri_commands::packs::desktop_pack_plugin_bind,
            tauri_commands::ux_plugins::desktop_ux_plugins_list,
            tauri_commands::ux_plugins::desktop_ux_plugin_source,
            tauri_commands::ux_plugins::desktop_plugin_call,
            tauri_commands::ux_plugins::desktop_ux_data_call,
            tauri_commands::ux_plugins::desktop_ux_data_surfaces,
            tauri_commands::packs::desktop_pack_wire,
            tauri_commands::packs::desktop_pack_wire_sources,
            tauri_commands::allowed_dirs::desktop_allowed_dirs_list,
            tauri_commands::allowed_dirs::desktop_allowed_dirs_add,
            tauri_commands::allowed_dirs::desktop_allowed_dirs_remove,
            tauri_commands::allowed_dirs::desktop_plugin_approvals_pending,
            tauri_commands::allowed_dirs::desktop_plugin_approval_decide,
            tauri_commands::allowed_dirs::desktop_plugin_approval_subscribe,
            tauri_commands::allowed_dirs::desktop_plugin_approval_unsubscribe,
            tauri_commands::lifecycle::desktop_bootstrap_summary,
            tauri_commands::e2e::desktop_native_e2e_config,
            tauri_commands::e2e::desktop_native_e2e_status,
            tauri_commands::lifecycle::desktop_init_local_standard,
            tauri_commands::lifecycle::desktop_client_start,
            tauri_commands::lifecycle::desktop_client_shutdown,
            tauri_commands::lifecycle::desktop_app_quit,
            tauri_commands::managed_server::desktop_managed_server_status,
            tauri_commands::managed_server::desktop_managed_server_start,
            tauri_commands::managed_server::desktop_managed_server_stop,
            tauri_commands::managed_server::desktop_managed_server_set_auto_start,
            tauri_commands::managed_server::desktop_managed_server_restart,
            tauri_commands::managed_server::desktop_managed_server_reset,
            tauri_commands::managed_server::desktop_managed_server_validate_root,
            tauri_commands::managed_server::desktop_managed_server_open_login_items,
            tauri_commands::db_explorer::desktop_open_db_explorer,
            tauri_commands::peers::desktop_peer_remove,
            tauri_commands::peers::desktop_peer_rename,
            tauri_commands::peers::desktop_peer_status_fetch,
            tauri_commands::peers::desktop_peer_enroll_status,
            tauri_commands::peers::desktop_p2p_repair,
            tauri_commands::workspace::desktop_workspace_list,
            tauri_commands::peers::desktop_network_status,
            tauri_commands::lifecycle::desktop_client_snapshot,
            tauri_commands::lifecycle::desktop_set_selected_node,
            tauri_commands::lifecycle::desktop_observer_metrics,
            tauri_commands::chat::desktop_session_snapshot,
            tauri_commands::chat::desktop_session_hydration_retry,
            tauri_commands::chat::desktop_session_live_delta,
            tauri_commands::chat::desktop_request_timeline,
            tauri_commands::tools_explain::desktop_tool_surface_explain,
            tauri_commands::chat::desktop_chat_send,
            tauri_commands::chat::desktop_pending_queue_edit,
            tauri_commands::chat::desktop_pending_message_steer,
            tauri_commands::chat::desktop_session_goal,
            tauri_commands::chat::desktop_goal_control,
            tauri_commands::chat::desktop_session_fork_before_message,
            tauri_commands::chat::desktop_session_rename,
            tauri_commands::chat::desktop_request_resend,
            tauri_commands::chat::desktop_request_retry,
            tauri_commands::mailbox::desktop_mailbox_list,
            tauri_commands::mailbox::desktop_mailbox_start_request,
            tauri_commands::mailbox::desktop_mailbox_dismiss,
            tauri_commands::config::desktop_node_config_save,
            tauri_commands::config::desktop_default_agent_set,
            tauri_commands::config::desktop_config_components_apply,
            tauri_commands::config::desktop_config_components_patch,
            tauri_commands::config::desktop_agent_save,
            tauri_commands::config::desktop_skill_save,
            tauri_commands::config::desktop_skill_delete,
            tauri_commands::config::desktop_task_delete,
            tauri_commands::config::desktop_schedule_delete,
            tauri_commands::config::desktop_trigger_delete,
            tauri_commands::config::desktop_backend_delete,
            tauri_commands::config::desktop_inference_profile_delete,
            tauri_commands::config::desktop_tools_delete,
            tauri_commands::config::desktop_tool_service_delete,
            tauri_commands::config::desktop_agent_delete,
            tauri_commands::config::desktop_context_delete,
            tauri_commands::config::desktop_backend_save,
            tauri_commands::config::desktop_inference_profile_save,
            tauri_commands::config::desktop_tools_save,
            tauri_commands::config::desktop_tool_service_save,
            tauri_commands::config::desktop_tool_service_test,
            tauri_commands::inference_setup::desktop_probe_inference_endpoint,
            tauri_commands::inference_setup::desktop_inference_setup_catalog,
            tauri_commands::inference_setup::desktop_inference_models_discover,
            tauri_commands::inference_setup::desktop_inference_model_recommendation,
            tauri_commands::inference_setup::desktop_inference_backend_recommendation,
            tauri_commands::inference_setup::desktop_codex_login,
            tauri_commands::inference_setup::desktop_codex_login_cancel,
            tauri_commands::inference_setup::desktop_grok_login,
            tauri_commands::inference_setup::desktop_grok_login_cancel,
            tauri_commands::inference_setup::desktop_claude_login,
            tauri_commands::inference_setup::desktop_claude_login_cancel,
            tauri_commands::inference_setup::desktop_provider_accounts_list,
            tauri_commands::inference_setup::desktop_provider_account_disconnect,
            tauri_commands::inference_setup::desktop_provider_account_retry_save,
            tauri_commands::inference_setup::desktop_provider_account_rename,
            tauri_commands::inference_setup::desktop_provider_account_remove,
            tauri_commands::inference_setup::desktop_provider_usage_read,
            tauri_commands::tasks::desktop_task_save,
            tauri_commands::tasks::desktop_schedule_save,
            tauri_commands::tasks::desktop_schedule_run,
            tauri_commands::tasks::desktop_trigger_save,
            tauri_commands::tasks::desktop_event_source_save,
            tauri_commands::tasks::desktop_event_source_delete,
            tauri_commands::tasks::desktop_task_run,
            tauri_commands::operations::desktop_operations_snapshot,
            tauri_commands::operations::desktop_session_provenance,
            tauri_commands::operations::desktop_interrupt_request,
            tauri_commands::operations::desktop_job_control,
            tauri_commands::operations::desktop_background_tool,
            tauri_commands::operations::desktop_list_backends_with_health,
            tauri_commands::operations::desktop_list_mcp_services_with_health,
            tauri_commands::operations::desktop_probe_mcp_service,
        ])
        .build()
}

fn register_window<R: Runtime>(webview: tauri::Webview<R>) {
    let window = webview.window();
    window
        .state::<DesktopAppState>()
        .register_window(window.label());
}

/// Holds the process lock across the reset reservation for the app lifetime.
fn hold_desktop_process<R: Runtime>(
    app: &tauri::AppHandle<R>,
    paths: &gents_desktop_core::client::DesktopPaths,
) -> anyhow::Result<()> {
    let reservation = gents_desktop_core::home_reset::lock_reset(paths)?;
    let process = gents_desktop_core::home_reset::lock_desktop_process(paths, false)?;
    anyhow::ensure!(
        app.manage(process),
        "the desktop process lock is already held by this app"
    );
    drop(reservation);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_desktop_core::client::DesktopPaths;
    use gents_desktop_core::home_reset::{
        lock_browser_store, lock_desktop_process, BrowserStoreLock, DesktopProcessLock,
    };

    #[test]
    fn browser_store_and_process_locks_are_both_held_after_setup() {
        let temp = tempfile::tempdir().unwrap();
        let paths = DesktopPaths::from_root(temp.path().join("desktop"));
        let store = format!("plugin-test-{}", std::process::id());
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        assert!(app.manage(lock_browser_store(Some(&store)).unwrap()));
        hold_desktop_process(app.handle(), &paths).unwrap();
        assert!(app.try_state::<BrowserStoreLock>().is_some());
        assert!(app.try_state::<DesktopProcessLock>().is_some());
        assert!(lock_browser_store(Some(&store)).is_err());
        assert!(lock_desktop_process(&paths, false).is_err());
    }

    #[test]
    fn native_views_register_before_selection_and_closed_view_ipc_is_ignored() {
        use tauri::ipc::InvokeBody;
        use tauri::webview::InvokeRequest;

        let temp = tempfile::tempdir().unwrap();
        let config = BridgeConfig {
            home: crate::config::HomePolicy::FixedRoot(temp.path().join("desktop")),
            bootstrap: crate::config::BootstrapPolicy::PairedRemoteOnly,
            ..Default::default()
        };
        let app = tauri::test::mock_builder()
            .manage(DesktopAppState::new(resolve_policy(&config, None).unwrap()))
            .plugin(
                Builder::<tauri::test::MockRuntime>::new("window-selection-test")
                    .on_webview_ready(register_window)
                    .build(),
            )
            .invoke_handler(tauri::generate_handler![
                tauri_commands::lifecycle::desktop_set_selected_node
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
        for label in ["main", "second"] {
            assert!(state.bridge.lock().unwrap().has_window(label));
        }
        main.hide().unwrap();
        state.window_event("main", &tauri::WindowEvent::Focused(false));
        assert!(state.bridge.lock().unwrap().has_window("main"));

        let invoke = |view: &tauri::WebviewWindow<tauri::test::MockRuntime>| {
            tauri::test::get_ipc_response(
                view,
                InvokeRequest {
                    cmd: "desktop_set_selected_node".into(),
                    callback: tauri::ipc::CallbackFn(0),
                    error: tauri::ipc::CallbackFn(1),
                    url: "http://tauri.localhost".parse().unwrap(),
                    body: InvokeBody::Json(serde_json::json!({"nodeDid": "did:stale"})),
                    headers: Default::default(),
                    invoke_key: tauri::test::INVOKE_KEY.to_string(),
                },
            )
        };
        assert!(invoke(&main)
            .expect_err("a live view still requires a running client")
            .to_string()
            .contains("desktop client not initialized"));
        state.window_event("second", &tauri::WindowEvent::Destroyed);
        invoke(&second).expect("IPC already queued by a destroyed view is ignored");
        assert!(!state.bridge.lock().unwrap().has_window("second"));
        assert!(state.bridge.lock().unwrap().has_window("main"));
    }
}
