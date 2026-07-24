use serde::{Deserialize, Serialize};

const E2E_STATUS_FILENAME: &str = "native-e2e-status.json";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeE2eConfig {
    agent_label: String,
    pair_token: String,
    prompt: String,
    expected_response: String,
    expect_empty_conversation_slice: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeE2eStatus {
    stage: String,
    detail: Option<String>,
}

#[tauri::command]
pub(crate) fn desktop_native_e2e_config() -> Result<Option<NativeE2eConfig>, String> {
    #[cfg(not(debug_assertions))]
    {
        Ok(None)
    }

    #[cfg(debug_assertions)]
    {
        if std::env::var("GENTS_NATIVE_E2E").ok().as_deref() != Some("1") {
            return Ok(None);
        }

        let pair_token = std::env::var("GENTS_E2E_PAIR_TOKEN")
            .map_err(|_| "GENTS_E2E_PAIR_TOKEN is required for native E2E".to_owned())?;
        if !pair_token.starts_with("dabear1-") {
            return Err("GENTS_E2E_PAIR_TOKEN is not a bearer pairing invite".to_owned());
        }

        Ok(Some(NativeE2eConfig {
            agent_label: std::env::var("GENTS_E2E_AGENT_LABEL")
                .unwrap_or_else(|_| "Fleet E2E Agent".to_owned()),
            pair_token,
            prompt: std::env::var("GENTS_E2E_PROMPT").unwrap_or_else(|_| {
                "Reply with only the uppercase underscore form of: fleet iphone simulator e2e."
                    .to_owned()
            }),
            expected_response: std::env::var("GENTS_E2E_EXPECTED_RESPONSE")
                .unwrap_or_else(|_| "FLEET_IPHONE_SIMULATOR_E2E".to_owned()),
            expect_empty_conversation_slice: std::env::var("GENTS_E2E_EXPECT_EMPTY_CONVERSATIONS")
                .ok()
                .as_deref()
                == Some("1"),
        }))
    }
}

#[tauri::command]
pub(crate) async fn desktop_native_e2e_status(status: NativeE2eStatus) -> Result<(), String> {
    #[cfg(not(debug_assertions))]
    {
        let _ = status;
        Err("native E2E status is disabled in release builds".to_owned())
    }

    #[cfg(debug_assertions)]
    {
        if std::env::var("GENTS_NATIVE_E2E").ok().as_deref() != Some("1") {
            return Err("native E2E status is not enabled".to_owned());
        }
        if status.stage.is_empty() || status.stage.len() > 64 {
            return Err("native E2E stage must contain 1-64 bytes".to_owned());
        }
        if status
            .detail
            .as_ref()
            .is_some_and(|detail| detail.len() > 2048)
        {
            return Err("native E2E detail must not exceed 2048 bytes".to_owned());
        }

        let directory = std::env::temp_dir();
        tokio::fs::create_dir_all(&directory)
            .await
            .map_err(|error| format!("creating native E2E status directory: {error}"))?;

        let bytes = serde_json::to_vec(&status)
            .map_err(|error| format!("serializing native E2E status: {error}"))?;
        let status_path = directory.join(E2E_STATUS_FILENAME);
        let temporary_path = directory.join(format!("{E2E_STATUS_FILENAME}.tmp"));
        tokio::fs::write(&temporary_path, bytes)
            .await
            .map_err(|error| format!("writing native E2E status: {error}"))?;
        tokio::fs::rename(&temporary_path, &status_path)
            .await
            .map_err(|error| format!("publishing native E2E status: {error}"))
    }
}
