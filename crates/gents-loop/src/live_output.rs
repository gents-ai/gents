use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Context;
use gents_protocol::output::{PayloadPresentation, PresentationPart};
use tokio::sync::Mutex;

/// Rendered between stdout and stderr when a live buffer's combined stream
/// starts carrying stderr bytes. Matches the finished-result renderer in
/// `gents::background_tools`, which imports this same constant.
pub const STDERR_BOUNDARY: &str = "\n--- stderr ---\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveOutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Default)]
pub struct LiveToolOutputRegistry {
    /// Execution registration only. Payload bytes are canonical segments, not
    /// a volatile ring store.
    inner: Arc<Mutex<HashMap<String, LiveToolOutputState>>>,
}

#[derive(Debug, Default)]
struct LiveToolOutputState {
    /// Channel-to-canonical byte coordinates, never captured payload bytes.
    receipts: [Vec<OutputReceipt>; 2],
    prepared: Option<PreparedToolPresentation>,
}

#[derive(Debug, Clone)]
struct OutputReceipt {
    channel_start: u64,
    channel_end: u64,
    source_start: u64,
    source_end: u64,
}

#[derive(Debug, Clone)]
pub struct PreparedToolPresentation {
    pub expected_text: String,
    pub presentation: PayloadPresentation,
}

/// Native output append authority supplied by the host. The guest registry
/// tracks receipts and presentation without depending on DefraDB.
pub trait CanonicalOutputAppender: std::fmt::Debug + Send + Sync {
    fn append<'a>(
        &'a self,
        text: &'a str,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<std::ops::Range<u64>>> + Send + 'a>>;
}

impl LiveToolOutputRegistry {
    pub async fn canonical_writer_for(
        &self,
        tool_call_doc_id: String,
        appender: Arc<dyn CanonicalOutputAppender>,
    ) -> LiveToolOutputWriter {
        let tool_call_id = tool_call_doc_id;
        self.inner
            .lock()
            .await
            .entry(tool_call_id.clone())
            .or_default();
        LiveToolOutputWriter {
            registry: self.clone(),
            tool_call_id,
            canonical: Some(appender),
            append_gate: Arc::new(Mutex::new(())),
            pending_utf8: Arc::new(Mutex::new(std::array::from_fn(|_| Vec::new()))),
        }
    }

    pub async fn writer_for(&self, tool_call_id: impl Into<String>) -> LiveToolOutputWriter {
        let tool_call_id = tool_call_id.into();
        self.inner
            .lock()
            .await
            .entry(tool_call_id.clone())
            .or_default();
        LiveToolOutputWriter {
            registry: self.clone(),
            tool_call_id,
            canonical: None,
            append_gate: Arc::new(Mutex::new(())),
            pending_utf8: Arc::new(Mutex::new(std::array::from_fn(|_| Vec::new()))),
        }
    }

    /// Ids of every tool call currently holding a live buffer.
    pub async fn live_ids(&self) -> Vec<String> {
        self.inner.lock().await.keys().cloned().collect()
    }

    pub async fn snapshot(&self, tool_call_id: &str) -> Option<LiveToolOutputSnapshot> {
        self.inner
            .lock()
            .await
            .contains_key(tool_call_id)
            .then(LiveToolOutputSnapshot::empty)
    }

    pub async fn remove(&self, tool_call_id: &str) {
        self.inner.lock().await.remove(tool_call_id);
    }

    async fn append(&self, tool_call_id: &str, stream: LiveOutputStream, bytes: &[u8]) {
        let _ = (tool_call_id, stream, bytes);
    }

    async fn record_receipt(
        &self,
        tool_call_id: &str,
        stream: LiveOutputStream,
        source: std::ops::Range<u64>,
    ) -> anyhow::Result<()> {
        if source.is_empty() {
            return Ok(());
        }
        let mut live = self.inner.lock().await;
        let state = live.get_mut(tool_call_id).ok_or_else(|| {
            anyhow::anyhow!("canonical output registration disappeared before receipt")
        })?;
        let receipts = &mut state.receipts[stream.index()];
        let channel_start = receipts
            .last()
            .map(|receipt| receipt.channel_end)
            .unwrap_or(0);
        let bytes = source
            .end
            .checked_sub(source.start)
            .context("canonical output receipt underflow")?;
        receipts.push(OutputReceipt {
            channel_start,
            channel_end: channel_start + bytes,
            source_start: source.start,
            source_end: source.end,
        });
        Ok(())
    }

    pub async fn take_prepared_presentation(
        &self,
        tool_call_id: &str,
        expected_text: &str,
    ) -> anyhow::Result<Option<PayloadPresentation>> {
        let mut live = self.inner.lock().await;
        let Some(state) = live.get_mut(tool_call_id) else {
            return Ok(None);
        };
        let Some(prepared) = state.prepared.take() else {
            return Ok(None);
        };
        anyhow::ensure!(
            prepared.expected_text == expected_text,
            "prepared tool presentation does not match the terminal native text"
        );
        Ok(Some(prepared.presentation))
    }

    async fn prepare_command_presentation(
        &self,
        tool_call_id: &str,
        expected_text: &str,
        metadata_json: &str,
        stdout_raw: &str,
        stdout_rendered: &str,
        stdout_returned_bytes: usize,
        stderr_raw: &str,
        stderr_rendered: &str,
        stderr_returned_bytes: usize,
        raw_json: bool,
    ) -> anyhow::Result<()> {
        let mut live = self.inner.lock().await;
        let state = live.get_mut(tool_call_id).ok_or_else(|| {
            anyhow::anyhow!("canonical output registration disappeared before presentation")
        })?;
        anyhow::ensure!(
            channel_len(&state.receipts[LiveOutputStream::Stdout.index()])
                == stdout_raw.len() as u64
                && channel_len(&state.receipts[LiveOutputStream::Stderr.index()])
                    == stderr_raw.len() as u64,
            "command capture receipts do not cover the complete raw stdout/stderr"
        );
        let stdout = channel_plan(
            stdout_raw,
            stdout_rendered,
            stdout_returned_bytes,
            !raw_json,
        )?;
        let stderr = channel_plan(
            stderr_raw,
            stderr_rendered,
            stderr_returned_bytes,
            !raw_json,
        )?;
        let mut parts = Vec::new();
        if raw_json {
            anyhow::ensure!(
                metadata_json.starts_with('{') && metadata_json.ends_with('}'),
                "command metadata did not serialize as a JSON object"
            );
            let metadata_fields = &metadata_json[1..metadata_json.len() - 1];
            push_literal(&mut parts, format!(r#"{{{metadata_fields},"stdout":"#));
            append_json_channel(&mut parts, stdout_raw, &stdout, &state.receipts[0])?;
            push_literal(&mut parts, r#"","stderr":"#.to_owned());
            append_json_channel(&mut parts, stderr_raw, &stderr, &state.receipts[1])?;
            push_literal(&mut parts, "\"}".to_owned());
        } else {
            push_literal(
                &mut parts,
                format!("gents_exec: {metadata_json}\nstdout:\n"),
            );
            append_plain_channel(&mut parts, &stdout, &state.receipts[0])?;
            push_literal(&mut parts, "\nstderr:\n".to_owned());
            append_plain_channel(&mut parts, &stderr, &state.receipts[1])?;
        }
        let presentation = PayloadPresentation::Composed { parts };
        state.prepared = Some(PreparedToolPresentation {
            expected_text: expected_text.to_owned(),
            presentation,
        });
        Ok(())
    }
}

#[derive(Clone)]
enum ChannelPart {
    Range { start: u64, end: u64 },
    Literal(String),
}

fn channel_len(receipts: &[OutputReceipt]) -> u64 {
    receipts
        .last()
        .map(|receipt| receipt.channel_end)
        .unwrap_or(0)
}

fn channel_plan(
    raw: &str,
    rendered: &str,
    returned_bytes: usize,
    empty_marker: bool,
) -> anyhow::Result<Vec<ChannelPart>> {
    anyhow::ensure!(
        returned_bytes <= raw.len() && raw.is_char_boundary(returned_bytes),
        "command truncation returned an invalid UTF-8 range"
    );
    anyhow::ensure!(
        rendered.starts_with(&raw[..returned_bytes]),
        "command truncation did not preserve its declared raw prefix"
    );
    let mut parts = Vec::new();
    if returned_bytes > 0 {
        parts.push(ChannelPart::Range {
            start: 0,
            end: returned_bytes as u64,
        });
    }
    if rendered.len() > returned_bytes {
        parts.push(ChannelPart::Literal(rendered[returned_bytes..].to_owned()));
    }
    if parts.is_empty() && empty_marker {
        parts.push(ChannelPart::Literal("(empty)".to_owned()));
    }
    Ok(parts)
}

fn push_literal(parts: &mut Vec<PresentationPart>, text: String) {
    if !text.is_empty() {
        parts.push(PresentationPart::Literal { text });
    }
}

fn append_plain_channel(
    output: &mut Vec<PresentationPart>,
    plan: &[ChannelPart],
    receipts: &[OutputReceipt],
) -> anyhow::Result<()> {
    for part in plan {
        match part {
            ChannelPart::Range { start, end } => {
                append_channel_range(output, receipts, *start, *end)?
            }
            ChannelPart::Literal(text) => push_literal(output, text.clone()),
        }
    }
    Ok(())
}

fn append_channel_range(
    output: &mut Vec<PresentationPart>,
    receipts: &[OutputReceipt],
    start: u64,
    end: u64,
) -> anyhow::Result<()> {
    let mut cursor = start;
    for receipt in receipts {
        let from = cursor.max(receipt.channel_start);
        let until = end.min(receipt.channel_end);
        if from >= until {
            continue;
        }
        let source_start = receipt.source_start + (from - receipt.channel_start);
        let source_end = receipt.source_start + (until - receipt.channel_start);
        anyhow::ensure!(
            source_end <= receipt.source_end,
            "command receipt range exceeds canonical chunk"
        );
        output.push(PresentationPart::OutputRange {
            start_byte: source_start,
            end_byte: source_end,
        });
        cursor = until;
    }
    anyhow::ensure!(
        cursor == end,
        "command presentation range has a receipt gap"
    );
    Ok(())
}

fn append_json_channel(
    output: &mut Vec<PresentationPart>,
    raw: &str,
    plan: &[ChannelPart],
    receipts: &[OutputReceipt],
) -> anyhow::Result<()> {
    for part in plan {
        match part {
            ChannelPart::Literal(text) => push_literal(output, json_fragment(text)?),
            ChannelPart::Range { start, end } => {
                let start = usize::try_from(*start)?;
                let end = usize::try_from(*end)?;
                let selected = &raw[start..end];
                for (offset, ch) in selected.char_indices() {
                    let next = offset + ch.len_utf8();
                    let encoded = json_fragment(&ch.to_string())?;
                    if encoded == ch.to_string() {
                        append_channel_range(
                            output,
                            receipts,
                            start as u64 + offset as u64,
                            start as u64 + next as u64,
                        )?;
                    } else {
                        push_literal(output, encoded);
                    }
                }
            }
        }
    }
    Ok(())
}

fn json_fragment(text: &str) -> anyhow::Result<String> {
    let encoded = serde_json::to_string(text)?;
    Ok(encoded[1..encoded.len() - 1].to_owned())
}

impl LiveOutputStream {
    const fn index(self) -> usize {
        match self {
            Self::Stdout => 0,
            Self::Stderr => 1,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LiveToolOutputWriter {
    registry: LiveToolOutputRegistry,
    tool_call_id: String,
    canonical: Option<Arc<dyn CanonicalOutputAppender>>,
    append_gate: Arc<Mutex<()>>,
    /// A subprocess may split a Unicode scalar across OS reads.  Canonical
    /// segment runs count UTF-8 bytes, so never turn each read into a lossy
    /// string independently.
    pending_utf8: Arc<Mutex<[Vec<u8>; 2]>>,
}

impl LiveToolOutputWriter {
    pub async fn prepare_command_presentation(
        &self,
        expected_text: &str,
        metadata_json: &str,
        stdout_raw: &str,
        stdout_rendered: &str,
        stdout_returned_bytes: usize,
        stderr_raw: &str,
        stderr_rendered: &str,
        stderr_returned_bytes: usize,
        raw_json: bool,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.canonical.is_some(),
            "command presentation requires a canonical live-output writer"
        );
        self.registry
            .prepare_command_presentation(
                &self.tool_call_id,
                expected_text,
                metadata_json,
                stdout_raw,
                stdout_rendered,
                stdout_returned_bytes,
                stderr_raw,
                stderr_rendered,
                stderr_returned_bytes,
                raw_json,
            )
            .await
    }

    pub async fn append(&self, stream: LiveOutputStream, bytes: &[u8]) {
        if let Some(binding) = &self.canonical {
            let _serial = self.append_gate.lock().await;
            let text = {
                let mut pending = self.pending_utf8.lock().await;
                let pending = &mut pending[match stream {
                    LiveOutputStream::Stdout => 0,
                    LiveOutputStream::Stderr => 1,
                }];
                pending.extend_from_slice(bytes);
                match std::str::from_utf8(&pending) {
                    Ok(text) => {
                        let text = text.to_owned();
                        pending.clear();
                        text
                    }
                    Err(error) if error.error_len().is_none() => {
                        // Preserve only the incomplete tail for the next
                        // callback; the valid prefix is already a complete
                        // immutable output fact.
                        let valid = error.valid_up_to();
                        let text = std::str::from_utf8(&pending[..valid])
                            .expect("valid_up_to is valid UTF-8")
                            .to_owned();
                        pending.drain(..valid);
                        text
                    }
                    Err(_) => {
                        // Process output is modeled as text.  Invalid bytes
                        // have no lossless representation in this stream, but
                        // convert the *one combined buffer* once so a split
                        // scalar can never produce two replacement characters.
                        let text = String::from_utf8_lossy(&pending).into_owned();
                        pending.clear();
                        text
                    }
                }
            };
            match binding.append(&text).await {
                Ok(source) => {
                    if let Err(error) = self
                        .registry
                        .record_receipt(&self.tool_call_id, stream, source)
                        .await
                    {
                        tracing::error!(tool_call_doc_id = %self.tool_call_id, %error,
                            "canonical tool receipt recording failed");
                    }
                }
                Err(error) => tracing::error!(tool_call_doc_id = %self.tool_call_id, %error,
                    "canonical tool output append failed"),
            }
            return;
        }
        self.registry
            .append(&self.tool_call_id, stream, bytes)
            .await;
    }
}

#[derive(Debug, Clone)]
pub struct LiveToolOutputSnapshot {
    pub combined: LiveOutputStreamSnapshot,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct LiveOutputStreamSnapshot {
    pub bytes: Vec<u8>,
    pub first_offset: u64,
    pub total_bytes_seen: u64,
}

impl LiveToolOutputSnapshot {
    fn empty() -> Self {
        Self {
            combined: LiveOutputStreamSnapshot {
                bytes: Vec::new(),
                first_offset: 0,
                total_bytes_seen: 0,
            },
            stdout_bytes: 0,
            stderr_bytes: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(source: &str, parts: &[PresentationPart]) -> String {
        parts
            .iter()
            .map(|part| match part {
                PresentationPart::Literal { text } => text.clone(),
                PresentationPart::OutputRange {
                    start_byte,
                    end_byte,
                } => source[*start_byte as usize..*end_byte as usize].to_owned(),
            })
            .collect()
    }

    #[test]
    fn composed_command_presentation_keeps_interleaved_unicode_raw_bytes_once() {
        // Source order is capture order: stdout `é`, stderr `!`, stdout `x`.
        // The presentation selects channels in renderer order without copying
        // either channel's raw bytes into a terminal segment.
        let source = "é!x";
        let stdout = vec![
            OutputReceipt {
                channel_start: 0,
                channel_end: 2,
                source_start: 0,
                source_end: 2,
            },
            OutputReceipt {
                channel_start: 2,
                channel_end: 3,
                source_start: 3,
                source_end: 4,
            },
        ];
        let stderr = vec![OutputReceipt {
            channel_start: 0,
            channel_end: 1,
            source_start: 2,
            source_end: 3,
        }];
        let mut parts = Vec::new();
        append_plain_channel(
            &mut parts,
            &channel_plan("éx", "éx", 3, true).unwrap(),
            &stdout,
        )
        .unwrap();
        push_literal(&mut parts, "|".into());
        append_plain_channel(
            &mut parts,
            &channel_plan("!", "!", 1, true).unwrap(),
            &stderr,
        )
        .unwrap();
        assert_eq!(render(source, &parts), "éx|!");

        let mut json = Vec::new();
        append_json_channel(
            &mut json,
            "éx",
            &channel_plan("éx", "éx", 3, false).unwrap(),
            &stdout,
        )
        .unwrap();
        assert_eq!(render(source, &json), "éx");
    }

    #[tokio::test]
    async fn writer_registration_is_visible_before_first_output_byte() {
        let registry = LiveToolOutputRegistry::default();
        let _writer = registry.writer_for("tool-1").await;

        let snapshot = registry
            .snapshot("tool-1")
            .await
            .expect("writer_for must eagerly register an empty live buffer");
        assert_eq!(snapshot.combined.total_bytes_seen, 0);
        assert!(snapshot.combined.bytes.is_empty());
    }
}
