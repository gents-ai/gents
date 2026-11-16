use serde::{Deserialize, Serialize};

use crate::error::BridgeError;

#[cfg(feature = "native-e2e")]
const E2E_STATUS_FILENAME: &str = "native-e2e-status.json";
#[cfg(feature = "native-e2e")]
const E2E_EVENTS_FILENAME: &str = "native-e2e-events.jsonl";

#[derive(Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct NativeE2eConfig {
    agent_label: String,
    server_address: String,
    prompt: String,
    expected_response: String,
    expect_empty_session_slice: bool,
    correlation_id: String,
    reconnect_real_inference: bool,
    measure_performance: bool,
    local_setup: Option<NativeLocalSetup>,
}

#[derive(Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct NativeLocalSetup {
    endpoint: String,
    model: String,
    phase: String,
    tool_root: String,
}

#[derive(Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct NativeE2eStatus {
    stage: String,
    detail: Option<String>,
    correlation_id: Option<String>,
    monotonic_ms: Option<f64>,
    metrics: Option<serde_json::Value>,
}

/// A native-e2e build's launch configuration, which the app's entry point
/// decodes once from its `--native-e2e <json>` argument and manages as state.
/// Without that argument the app runs as an ordinary desktop.
#[cfg(feature = "native-e2e")]
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeE2eLaunch {
    server_address: String,
    #[serde(default = "default_agent_label")]
    agent_label: String,
    #[serde(default = "default_prompt")]
    prompt: String,
    #[serde(default = "default_expected_response")]
    expected_response: String,
    #[serde(default)]
    expect_empty_session_slice: bool,
    #[serde(default = "default_correlation_id")]
    correlation_id: String,
    #[serde(default)]
    reconnect_real_inference: bool,
    #[serde(default)]
    measure_performance: bool,
    /// Drives the local setup flow against an isolated node and desktop home.
    pub local: Option<NativeLocalLaunch>,
    /// 32 hex digits naming the isolated WebKit store the windows open.
    pub webview_store_id: Option<String>,
}

#[cfg(feature = "native-e2e")]
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeLocalLaunch {
    model: String,
    #[serde(default = "default_local_phase")]
    phase: String,
    #[serde(default)]
    tool_root: String,
    pub node_home: std::path::PathBuf,
    pub desktop_home: std::path::PathBuf,
    /// Keeps store keys in files rather than the platform keychain.
    #[serde(default)]
    pub file_store_keys: bool,
}

#[cfg(feature = "native-e2e")]
fn default_agent_label() -> String {
    "Fleet E2E Agent".to_owned()
}

#[cfg(feature = "native-e2e")]
fn default_prompt() -> String {
    "Reply with only the uppercase underscore form of: fleet iphone simulator e2e.".to_owned()
}

#[cfg(feature = "native-e2e")]
fn default_expected_response() -> String {
    "FLEET_IPHONE_SIMULATOR_E2E".to_owned()
}

#[cfg(feature = "native-e2e")]
fn default_correlation_id() -> String {
    "native-e2e".to_owned()
}

#[cfg(feature = "native-e2e")]
fn default_local_phase() -> String {
    "setup".to_owned()
}

#[cfg(feature = "native-e2e")]
impl NativeE2eLaunch {
    pub fn parse(raw: &str) -> anyhow::Result<Self> {
        use anyhow::Context;
        let launch: Self = serde_json::from_str(raw).context("decoding --native-e2e")?;
        anyhow::ensure!(
            !launch.server_address.trim().is_empty(),
            "--native-e2e serverAddress must not be empty"
        );
        if let Some(local) = &launch.local {
            anyhow::ensure!(
                local.node_home.is_absolute() && local.desktop_home.is_absolute(),
                "--native-e2e local nodeHome and desktopHome must be absolute"
            );
        }
        Ok(launch)
    }

    fn config(&self) -> NativeE2eConfig {
        NativeE2eConfig {
            agent_label: self.agent_label.clone(),
            server_address: self.server_address.clone(),
            prompt: self.prompt.clone(),
            expected_response: self.expected_response.clone(),
            expect_empty_session_slice: self.expect_empty_session_slice,
            correlation_id: self.correlation_id.clone(),
            reconnect_real_inference: self.reconnect_real_inference,
            measure_performance: self.measure_performance,
            local_setup: self.local.as_ref().map(|local| NativeLocalSetup {
                endpoint: self.server_address.clone(),
                model: local.model.clone(),
                phase: local.phase.clone(),
                tool_root: local.tool_root.clone(),
            }),
        }
    }
}

#[tauri::command]
#[cfg(feature = "native-e2e")]
pub fn desktop_native_e2e_config<R>(
    app: tauri::AppHandle<R>,
) -> Result<Option<NativeE2eConfig>, BridgeError>
where
    R: tauri::Runtime,
{
    #[cfg(not(debug_assertions))]
    {
        let _ = app;
        return Ok(None);
    }

    #[cfg(debug_assertions)]
    {
        use tauri::Manager;
        let launch = app.try_state::<NativeE2eLaunch>();
        tracing::info!(
            target: "gents_desktop_bridge::mobile_performance",
            enabled = launch.is_some(),
            "native E2E configuration requested"
        );
        Ok(launch.map(|launch| launch.config()))
    }
}

#[tauri::command]
#[cfg(feature = "native-e2e")]
pub async fn desktop_native_e2e_status<R>(
    app: tauri::AppHandle<R>,
    status: NativeE2eStatus,
) -> Result<(), BridgeError>
where
    R: tauri::Runtime,
{
    #[cfg(not(debug_assertions))]
    {
        let _ = (app, status);
        return Err(BridgeError::untyped(
            "native E2E status is disabled in release builds",
        ));
    }

    #[cfg(debug_assertions)]
    {
        use tauri::Manager;
        if app.try_state::<NativeE2eLaunch>().is_none() {
            return Err(BridgeError::untyped("native E2E status is not enabled"));
        }
        if status.stage.is_empty() || status.stage.len() > 64 {
            return Err(BridgeError::untyped(
                "native E2E stage must contain 1-64 bytes",
            ));
        }
        if status
            .detail
            .as_ref()
            .is_some_and(|detail| detail.len() > 2048)
        {
            return Err(BridgeError::untyped(
                "native E2E detail must not exceed 2048 bytes",
            ));
        }
        if status
            .correlation_id
            .as_ref()
            .is_some_and(|value| value.is_empty() || value.len() > 128)
        {
            return Err(BridgeError::untyped(
                "native E2E correlation id must contain 1-128 bytes",
            ));
        }
        if status
            .monotonic_ms
            .is_some_and(|value| !value.is_finite() || value < 0.0)
        {
            return Err(BridgeError::untyped(
                "native E2E monotonic time must be finite and nonnegative",
            ));
        }

        let directory = std::env::temp_dir();
        tokio::fs::create_dir_all(&directory)
            .await
            .map_err(|error| {
                BridgeError::untyped(format!("creating native E2E status directory: {error}"))
            })?;

        let bytes = serde_json::to_vec(&status).map_err(|error| {
            BridgeError::untyped(format!("serializing native E2E status: {error}"))
        })?;
        if bytes.len() > 32 * 1024 {
            return Err(BridgeError::untyped(
                "native E2E status must not exceed 32 KiB",
            ));
        }
        let status_path = directory.join(E2E_STATUS_FILENAME);
        let temporary_path = directory.join(format!("{E2E_STATUS_FILENAME}.tmp"));
        tokio::fs::write(&temporary_path, &bytes)
            .await
            .map_err(|error| BridgeError::untyped(format!("writing native E2E status: {error}")))?;
        tokio::fs::rename(&temporary_path, &status_path)
            .await
            .map_err(|error| {
                BridgeError::untyped(format!("publishing native E2E status: {error}"))
            })?;
        use tokio::io::AsyncWriteExt;
        let events_path = directory.join(E2E_EVENTS_FILENAME);
        let mut events = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&events_path)
            .await
            .map_err(|error| {
                BridgeError::untyped(format!("opening native E2E event artifact: {error}"))
            })?;
        events.write_all(&bytes).await.map_err(|error| {
            BridgeError::untyped(format!("writing native E2E event artifact: {error}"))
        })?;
        events.write_all(b"\n").await.map_err(|error| {
            BridgeError::untyped(format!("terminating native E2E event artifact: {error}"))
        })?;
        tracing::info!(
            target: "gents_desktop_bridge::mobile_performance",
            stage = %status.stage,
            correlation_id = status.correlation_id.as_deref().unwrap_or("none"),
            monotonic_ms = status.monotonic_ms.unwrap_or_default(),
            "native E2E observable boundary"
        );
        Ok(())
    }
}

#[tauri::command]
#[cfg(not(feature = "native-e2e"))]
pub fn desktop_native_e2e_config() -> Result<Option<NativeE2eConfig>, BridgeError> {
    Ok(None)
}

#[tauri::command]
#[cfg(not(feature = "native-e2e"))]
pub async fn desktop_native_e2e_status(status: NativeE2eStatus) -> Result<(), BridgeError> {
    let _ = status;
    Err(BridgeError::untyped(
        "native E2E commands are not compiled into this build",
    ))
}
