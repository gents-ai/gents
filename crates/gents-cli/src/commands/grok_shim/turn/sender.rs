use std::sync::Arc;

use anyhow::Result;
use serde_json::{json, Value};

use super::super::projection::{AsyncSendLine, SessionUpdateChannel, SESSION_UPDATE_METHOD};
use super::super::server::AcpOutbound;
use super::wire::PromptBlock;
#[cfg(test)]
use tokio::sync::Mutex;

#[derive(Clone)]
pub(in crate::commands::grok_shim) enum PromptSender {
    Live {
        outbound: AcpOutbound,
    },
    Replay {
        inner: Arc<PromptSender>,
    },
    #[cfg(test)]
    Buffer {
        buffer: Arc<Mutex<Vec<String>>>,
    },
}

impl PromptSender {
    pub(in crate::commands::grok_shim) async fn send_line(&self, line: String) -> Result<()> {
        match self {
            PromptSender::Live { outbound } => outbound.send(line).await,
            PromptSender::Replay { inner } => {
                let mut value: Value = serde_json::from_str(&line)?;
                value["params"]["_meta"]["isReplay"] = Value::Bool(true);
                Box::pin(inner.send_line(serde_json::to_string(&value)?)).await
            }
            #[cfg(test)]
            PromptSender::Buffer { buffer } => {
                buffer.lock().await.push(line);
                Ok(())
            }
        }
    }

    pub(in crate::commands::grok_shim) async fn take_lines(&self) -> Vec<String> {
        match self {
            PromptSender::Live { .. } => Vec::new(),
            PromptSender::Replay { inner } => Box::pin(inner.take_lines()).await,
            #[cfg(test)]
            PromptSender::Buffer { buffer } => {
                let mut buffer = buffer.lock().await;
                std::mem::take(&mut *buffer)
            }
        }
    }

    pub(in crate::commands::grok_shim) async fn send_user_message_chunk(
        &self,
        session_updates: &SessionUpdateChannel,
        session_id: &str,
        prompt_id: &str,
        block: &PromptBlock,
        prompt_index: usize,
    ) -> Result<()> {
        let session_id = session_id.to_string();
        let prompt_id = prompt_id.to_string();
        let block = block.clone();
        let session_for_lock = session_id.clone();
        session_updates
            .send(
                &session_for_lock,
                move |event_id, total_tokens| {
                    let meta = json!({
                        "promptId": prompt_id,
                        "isReplay": false,
                        "eventId": event_id,
                        "totalTokens": total_tokens,
                        "agentTimestampMs": chrono::Utc::now().timestamp_millis(),
                    });
                    let params = json!({
                        "sessionId": session_id,
                        "update": {
                            "sessionUpdate": "user_message_chunk",
                            "content": {
                                "type": "text",
                                "text": block.text,
                            },
                            "_meta": {
                                "promptIndex": prompt_index,
                                "hideFromScrollback": false,
                            },
                        },
                        "_meta": meta,
                    });
                    Ok(json!({
                        "jsonrpc": "2.0",
                        "method": SESSION_UPDATE_METHOD,
                        "params": params,
                    }))
                },
                PromptSenderLine(self),
            )
            .await
            .map(|_| ())
    }
}

pub(in crate::commands::grok_shim) struct PromptSenderLine<'a>(
    pub(in crate::commands::grok_shim) &'a PromptSender,
);

impl AsyncSendLine for PromptSenderLine<'_> {
    async fn send_line(&self, line: String) -> Result<()> {
        self.0.send_line(line).await
    }
}
