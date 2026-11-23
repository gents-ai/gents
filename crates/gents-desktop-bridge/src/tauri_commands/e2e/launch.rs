//! The launch-argument decoder of a native-e2e build. It is not a bridge
//! wire type, so it lives outside the bridge contract sources.

use serde::Deserialize;

use super::{NativeE2eConfig, NativeLocalSetup};

/// A native-e2e build's launch configuration, which the app's entry point
/// decodes once from its `--native-e2e <json>` argument and manages as state.
/// Without that argument the app runs as an ordinary desktop.
#[derive(Clone, Debug, Deserialize)]
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
    /// Drives the local setup flow against an isolated node home.
    pub local: Option<NativeLocalLaunch>,
    /// 32 hex digits naming the isolated WebKit store the windows open.
    pub webview_store_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeLocalLaunch {
    model: String,
    #[serde(default = "default_local_phase")]
    phase: String,
    #[serde(default)]
    tool_root: String,
    pub node_home: std::path::PathBuf,
    /// Keeps store keys in files rather than the platform keychain.
    #[serde(default)]
    pub file_store_keys: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum NativeE2eLaunchError {
    #[error("decoding --native-e2e: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("--native-e2e serverAddress must not be empty")]
    EmptyServerAddress,
    #[error("--native-e2e local nodeHome must be absolute")]
    RelativeNodeHome,
}

fn default_agent_label() -> String {
    "Fleet E2E Agent".to_owned()
}

fn default_prompt() -> String {
    "Reply with only the uppercase underscore form of: fleet iphone simulator e2e.".to_owned()
}

fn default_expected_response() -> String {
    "FLEET_IPHONE_SIMULATOR_E2E".to_owned()
}

fn default_correlation_id() -> String {
    "native-e2e".to_owned()
}

fn default_local_phase() -> String {
    "setup".to_owned()
}

impl NativeE2eLaunch {
    pub fn parse(raw: &str) -> Result<Self, NativeE2eLaunchError> {
        let launch: Self = serde_json::from_str(raw)?;
        if launch.server_address.trim().is_empty() {
            return Err(NativeE2eLaunchError::EmptyServerAddress);
        }
        if launch
            .local
            .as_ref()
            .is_some_and(|local| !local.node_home.is_absolute())
        {
            return Err(NativeE2eLaunchError::RelativeNodeHome);
        }
        Ok(launch)
    }

    pub(super) fn config(&self) -> NativeE2eConfig {
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
