use std::sync::Arc;

use gents_protocol::error_chain::ErrorChain;
use tokio::sync::Mutex;

use super::{
    CanonicalOutputAppender, CapturedChannel, CommandPresentationLayout, DeferredToolWrites,
    LiveOutputError, LiveOutputStream, LiveToolOutputRegistry, Result,
};

#[derive(Debug, Clone)]
pub struct LiveToolOutputWriter {
    pub(super) registry: LiveToolOutputRegistry,
    pub(super) tool_call_id: String,
    pub(super) canonical: Option<Arc<dyn CanonicalOutputAppender>>,
    pub(super) append_gate: Arc<Mutex<()>>,
    pub(super) pending_utf8: Arc<Mutex<[Vec<u8>; 2]>>,
}

impl LiveToolOutputWriter {
    /// The physical tool call this writer registers output for.
    pub fn tool_call_doc_id(&self) -> &str {
        &self.tool_call_id
    }

    pub async fn record_plugin_receipt(
        &self,
        receipt: gents_protocol::plugin::PluginExecutionReceipt,
    ) -> Result<()> {
        let mut states = self.registry.inner.lock().await;
        let state = states
            .get_mut(&self.tool_call_id)
            .ok_or(LiveOutputError::PluginReceiptUnregistered)?;
        if !state
            .plugin_receipt
            .as_ref()
            .is_none_or(|existing| existing == &receipt)
        {
            return Err(LiveOutputError::PluginReceiptConflict);
        }
        state.plugin_receipt = Some(receipt);
        Ok(())
    }

    pub async fn record_deferred_writes(&self, writes: DeferredToolWrites) -> Result<()> {
        let mut states = self.registry.inner.lock().await;
        let state = states
            .get_mut(&self.tool_call_id)
            .ok_or(LiveOutputError::DeferredWritesUnregistered)?;
        match &mut state.deferred_writes {
            Some(existing) => existing.0.extend(writes.0),
            None => state.deferred_writes = Some(writes),
        }
        Ok(())
    }

    pub async fn prepare_command_presentation(
        &self,
        expected_text: &str,
        layout: CommandPresentationLayout<'_>,
        stdout: CapturedChannel<'_>,
        stderr: CapturedChannel<'_>,
    ) -> Result<()> {
        if self.canonical.is_none() {
            return Err(LiveOutputError::CanonicalWriterRequired);
        }
        self.flush_pending().await;
        self.registry
            .prepare_command_presentation(&self.tool_call_id, expected_text, layout, stdout, stderr)
            .await
    }

    pub async fn append(&self, stream: LiveOutputStream, bytes: &[u8]) {
        if let Some(binding) = &self.canonical {
            let _serial = self.append_gate.lock().await;
            let text = {
                let mut pending = self.pending_utf8.lock().await;
                let pending = &mut pending[stream.index()];
                pending.extend_from_slice(bytes);
                drain_utf8(pending, false)
            };
            self.commit(binding.as_ref(), stream, &text).await;
            return;
        }
        self.registry
            .append(&self.tool_call_id, stream, bytes)
            .await;
    }

    async fn flush_pending(&self) {
        let Some(binding) = &self.canonical else {
            return;
        };
        let _serial = self.append_gate.lock().await;
        for stream in [LiveOutputStream::Stdout, LiveOutputStream::Stderr] {
            let text = {
                let mut pending = self.pending_utf8.lock().await;
                drain_utf8(&mut pending[stream.index()], true)
            };
            self.commit(binding.as_ref(), stream, &text).await;
        }
    }

    async fn commit(
        &self,
        binding: &dyn CanonicalOutputAppender,
        stream: LiveOutputStream,
        text: &str,
    ) {
        if text.is_empty() {
            return;
        }
        match binding.append(text).await {
            Ok(source) => {
                if let Err(error) = self
                    .registry
                    .record_receipt(&self.tool_call_id, stream, source)
                    .await
                {
                    tracing::error!(tool_call_doc_id = %self.tool_call_id,
                        error = %ErrorChain(&error),
                        "canonical tool receipt recording failed");
                }
            }
            Err(error) => tracing::error!(tool_call_doc_id = %self.tool_call_id,
                error = %ErrorChain(&error),
                "canonical tool output append failed"),
        }
    }
}

pub(super) fn drain_utf8(pending: &mut Vec<u8>, end_of_stream: bool) -> String {
    let mut text = String::new();
    let mut consumed = 0;
    while consumed < pending.len() {
        match std::str::from_utf8(&pending[consumed..]) {
            Ok(valid) => {
                text.push_str(valid);
                consumed = pending.len();
            }
            Err(error) => {
                let valid = error.valid_up_to();
                text.push_str(
                    std::str::from_utf8(&pending[consumed..consumed + valid])
                        .expect("valid_up_to is valid UTF-8"),
                );
                consumed += valid;
                match error.error_len() {
                    Some(invalid) => {
                        text.push(char::REPLACEMENT_CHARACTER);
                        consumed += invalid;
                    }
                    None if end_of_stream => {
                        text.push(char::REPLACEMENT_CHARACTER);
                        consumed = pending.len();
                    }
                    None => break,
                }
            }
        }
    }
    pending.drain(..consumed);
    text
}
