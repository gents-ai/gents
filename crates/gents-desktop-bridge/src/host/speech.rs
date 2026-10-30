use anyhow::{ensure, Context, Result};
use std::sync::OnceLock;

mod transport;
use gents_desktop_core::client::ClientCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use transport::{Operation, Request, Response, SpeechTransport};
use ts_rs::TS;

use gents_protocol::client_media::{MediaPcmClip, MAX_PCM_BYTES, MAX_SECONDS};

#[derive(Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SpeechServiceView {
    pub service_id: String,
    pub display_name: String,
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpeechToolReference {
    pub node_did: String,
    pub service_id: String,
    pub tool_name: String,
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
/// The selected MCP adapter accepts `audio` (base64 PCM data, format,
/// sampleRate and channels) and optional language. It returns `{ text }`
/// as structured content or one JSON text result.
pub struct SpeechRecognizeRequest {
    pub tool: SpeechToolReference,
    pub clip: MediaPcmClip,
    #[ts(optional = nullable)]
    pub language: Option<String>,
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
/// The selected MCP adapter accepts text, format, sampleRate, channels,
/// maxSeconds, maxPcmBytes and optional language/voice. It returns `{ pcm, sampleRate }`
/// containing mono signed 16-bit little-endian PCM. Credentials belong to
/// that service, never to a client preference or tool argument.
pub struct SpeechSynthesizeRequest {
    pub tool: SpeechToolReference,
    pub text: String,
    pub sample_rate: u32,
    #[ts(optional = nullable)]
    pub language: Option<String>,
    #[ts(optional = nullable)]
    pub voice: Option<String>,
}

#[derive(Default)]
pub(super) struct SpeechClient {
    transport: OnceLock<Result<SpeechTransport, String>>,
}

impl SpeechClient {
    pub async fn services(
        &self,
        core: &ClientCore,
        node_did: &str,
    ) -> Result<Vec<SpeechServiceView>> {
        Ok(services(core, node_did)
            .await?
            .into_iter()
            .filter(|service| service.enabled)
            .map(|service| SpeechServiceView {
                display_name: service
                    .display_name
                    .unwrap_or_else(|| service.service_id.clone()),
                service_id: service.service_id,
            })
            .collect())
    }

    pub async fn tools(
        &self,
        core: &ClientCore,
        node_did: &str,
        service_id: &str,
    ) -> Result<Vec<crate::types::ToolServiceToolView>> {
        let route = route(core, node_did, service_id).await?;
        let result = self
            .transport()?
            .request(Request {
                service: service_id.to_owned(),
                endpoint: route.endpoint,
                actor: core.node_identity().did().to_owned(),
                send_node_did: route.send_node_did,
                operation: Operation::List,
            })
            .await?;
        match result {
            Response::Tools(tools) => Ok(tools),
            Response::Value(_) => anyhow::bail!("The speech worker returned an invalid tool list."),
        }
    }

    pub async fn recognize(
        &self,
        core: &ClientCore,
        request: SpeechRecognizeRequest,
    ) -> Result<String> {
        request.clip.validate()?;
        validate_label(request.language.as_deref())?;
        let mut arguments = json!({
            "audio": {"data":request.clip.pcm, "format":"pcm_s16le", "sampleRate":request.clip.sample_rate, "channels":1},
        });
        if let Some(language) = request.language {
            arguments["language"] = json!(language);
        }
        let result = self.call(core, &request.tool, arguments).await?;
        let text = result
            .get("text")
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty() && text.len() <= 64 * 1024)
            .context(
                "The transcription tool must return a nonempty text field of at most 64 KiB.",
            )?;
        Ok(text.to_owned())
    }

    pub async fn synthesize(
        &self,
        core: &ClientCore,
        request: SpeechSynthesizeRequest,
    ) -> Result<MediaPcmClip> {
        ensure!(
            !request.text.trim().is_empty() && request.text.len() <= 4096,
            "Choose a speech segment of at most 4 KiB."
        );
        ensure!(
            (8_000..=192_000).contains(&request.sample_rate),
            "The playback sample rate is unsupported."
        );
        validate_label(request.language.as_deref())?;
        validate_label(request.voice.as_deref())?;
        let mut arguments = json!({
            "text":request.text,"format":"pcm_s16le","sampleRate":request.sample_rate,"channels":1,
            "maxSeconds":MAX_SECONDS.min((MAX_PCM_BYTES / 2 / request.sample_rate as usize) as u32),
            "maxPcmBytes":MAX_PCM_BYTES,
        });
        if let Some(language) = request.language {
            arguments["language"] = json!(language);
        }
        if let Some(voice) = request.voice {
            arguments["voice"] = json!(voice);
        }
        let result = self.call(core, &request.tool, arguments).await?;
        let clip: MediaPcmClip = serde_json::from_value(result).map_err(|_| {
            anyhow::anyhow!(
                "The speech tool must return pcm and sampleRate fields for mono signed 16-bit PCM."
            )
        })?;
        clip.validate()?;
        ensure!(clip.sample_rate == request.sample_rate, "The speech tool returned audio at a different sample rate. Check its speech adapter configuration.");
        Ok(clip)
    }

    /// Speech uses operator-selected configured tools outside the durable
    /// request journal. The outbound DID is the actual client actor. Service
    /// errors are deliberately not surfaced: providers can echo audio inputs.
    async fn call(
        &self,
        core: &ClientCore,
        tool: &SpeechToolReference,
        arguments: Value,
    ) -> Result<Value> {
        ensure!(
            !tool.tool_name.trim().is_empty() && tool.tool_name.len() <= 256,
            "Choose a configured speech tool."
        );
        let route = route(core, &tool.node_did, &tool.service_id).await?;
        let result = self
            .transport()?
            .request(Request {
                service: tool.service_id.clone(),
                endpoint: route.endpoint,
                actor: core.node_identity().did().to_owned(),
                send_node_did: route.send_node_did,
                operation: Operation::Call {
                    tool: tool.tool_name.clone(),
                    arguments,
                },
            })
            .await?;
        match result {
            Response::Value(value) => Ok(value),
            Response::Tools(_) => {
                anyhow::bail!("The speech worker returned an invalid tool result.")
            }
        }
    }

    fn transport(&self) -> Result<&SpeechTransport> {
        self.transport
            .get_or_init(|| {
                SpeechTransport::new().map_err(|_| {
                    "The speech worker could not start. Restart the Gents client.".to_owned()
                })
            })
            .as_ref()
            .map_err(|error| anyhow::anyhow!(error.clone()))
    }
}

async fn services(
    core: &ClientCore,
    node_did: &str,
) -> Result<Vec<gents::documents::ToolServiceRegistry>> {
    let records = core.peer_records().await;
    ensure!(records.iter().filter(|record| record.node_did == node_did).count() == 1,
        "Choose a connected runtime with an operator configuration route to configure speech services.");
    let access = core.operator_graphql(node_did).map(gents::ConfigAccess::Graphql).context(
        "Speech configuration cannot be read for this runtime. Use Gents: Connect Local Runtime on this machine, or choose another service runtime."
    )?;
    gents::registry::configured_mcp_services_on(&access, node_did).await
}

async fn route(
    core: &ClientCore,
    node_did: &str,
    service_id: &str,
) -> Result<gents::registry::ResolvedMcpService> {
    let service = services(core, node_did).await?.into_iter().find(|service| service.enabled && service.service_id == service_id)
        .context("The selected speech service is no longer enabled for this runtime. Open Gents voice settings.")?;
    let hostname = hostname::get()?.to_string_lossy().into_owned();
    gents::registry::resolve_mcp_service(&service, &hostname, None)
}

fn validate_label(value: Option<&str>) -> Result<()> {
    ensure!(value.is_none_or(|value| value.len() <= 256 && !value.chars().any(char::is_control)), "Speech language and voice identifiers must be at most 256 bytes and contain no control characters.");
    Ok(())
}
