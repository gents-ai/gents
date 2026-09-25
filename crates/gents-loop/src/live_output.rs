use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Context;
use gents_protocol::output::{PayloadPresentation, PresentationPart};
use tokio::sync::Mutex;

/// Compact command-runner output starts with this prefix and one metadata
/// JSON object on its own line, followed by the captured streams.
pub const COMMAND_OUTPUT_META_PREFIX: &str = "gents_exec: ";

/// Command metadata fields that differ between runs of the same command:
/// wall-clock duration, and the foreground-timeout hint rendered from it.
pub const COMMAND_OUTPUT_VOLATILE_FIELDS: [&str; 2] = ["duration_ms", "hint"];

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
        layout: CommandPresentationLayout<'_>,
        stdout: CapturedChannel<'_>,
        stderr: CapturedChannel<'_>,
    ) -> anyhow::Result<()> {
        let mut live = self.inner.lock().await;
        let state = live.get_mut(tool_call_id).ok_or_else(|| {
            anyhow::anyhow!("canonical output registration disappeared before presentation")
        })?;
        anyhow::ensure!(
            channel_len(&state.receipts[LiveOutputStream::Stdout.index()])
                == stdout.raw.len() as u64
                && channel_len(&state.receipts[LiveOutputStream::Stderr.index()])
                    == stderr.raw.len() as u64,
            "command capture receipts do not cover the complete raw stdout/stderr"
        );
        let json_object = matches!(layout, CommandPresentationLayout::JsonObject { .. });
        let stdout_plan = channel_plan(&stdout, !json_object)?;
        let stderr_plan = channel_plan(&stderr, !json_object)?;
        let mut parts = Vec::new();
        match layout {
            CommandPresentationLayout::JsonObject { metadata_json } => {
                anyhow::ensure!(
                    metadata_json.starts_with('{') && metadata_json.ends_with('}'),
                    "command metadata did not serialize as a JSON object"
                );
                let metadata_fields = &metadata_json[1..metadata_json.len() - 1];
                push_literal(&mut parts, format!(r#"{{{metadata_fields},"stdout":""#));
                append_json_channel(&mut parts, stdout.raw, &stdout_plan, &state.receipts[0])?;
                push_literal(&mut parts, r#"","stderr":""#.to_owned());
                append_json_channel(&mut parts, stderr.raw, &stderr_plan, &state.receipts[1])?;
                push_literal(&mut parts, "\"}".to_owned());
            }
            CommandPresentationLayout::Labeled {
                head,
                json_string: false,
            } => {
                push_literal(&mut parts, format!("{head}stdout:\n"));
                append_plain_channel(&mut parts, &stdout_plan, &state.receipts[0])?;
                push_literal(&mut parts, "\nstderr:\n".to_owned());
                append_plain_channel(&mut parts, &stderr_plan, &state.receipts[1])?;
            }
            CommandPresentationLayout::Labeled {
                head,
                json_string: true,
            } => {
                push_literal(
                    &mut parts,
                    format!("\"{}", json_fragment(&format!("{head}stdout:\n"))?),
                );
                append_json_channel(&mut parts, stdout.raw, &stdout_plan, &state.receipts[0])?;
                push_literal(&mut parts, json_fragment("\nstderr:\n")?);
                append_json_channel(&mut parts, stderr.raw, &stderr_plan, &state.receipts[1])?;
                push_literal(&mut parts, "\"".to_owned());
            }
        }
        let presentation = PayloadPresentation::Composed { parts };
        state.prepared = Some(PreparedToolPresentation {
            expected_text: expected_text.to_owned(),
            presentation,
        });
        Ok(())
    }
}

/// How a finished command's text frames its two captured channels.
#[derive(Debug, Clone, Copy)]
pub enum CommandPresentationLayout<'a> {
    /// One JSON object: the metadata object's fields, then `stdout` and
    /// `stderr` string fields.
    JsonObject { metadata_json: &'a str },
    /// `{head}stdout:\n{stdout}\nstderr:\n{stderr}`, with `(empty)` for an
    /// empty channel; `json_string` encodes the whole text as one JSON string.
    Labeled { head: &'a str, json_string: bool },
}

/// One captured command channel: its complete raw text, the text shown for
/// it, and the byte range of `raw` shown verbatim. The range is a prefix or a
/// suffix of `raw`; anything else in `rendered` is a literal notice before a
/// suffix or after a prefix.
#[derive(Debug, Clone)]
pub struct CapturedChannel<'a> {
    pub raw: &'a str,
    pub rendered: &'a str,
    pub shown: std::ops::Range<usize>,
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
    channel: &CapturedChannel<'_>,
    empty_marker: bool,
) -> anyhow::Result<Vec<ChannelPart>> {
    let CapturedChannel {
        raw,
        rendered,
        shown,
    } = channel;
    anyhow::ensure!(
        shown.start <= shown.end
            && shown.end <= raw.len()
            && raw.is_char_boundary(shown.start)
            && raw.is_char_boundary(shown.end)
            && (shown.start == 0 || shown.end == raw.len()),
        "command truncation returned an invalid UTF-8 range"
    );
    let kept = &raw[shown.clone()];
    let (before, after) = if shown.start == 0 {
        anyhow::ensure!(
            rendered.starts_with(kept),
            "command truncation did not preserve its declared raw prefix"
        );
        ("", &rendered[kept.len()..])
    } else {
        anyhow::ensure!(
            rendered.ends_with(kept),
            "command truncation did not preserve its declared raw suffix"
        );
        (&rendered[..rendered.len() - kept.len()], "")
    };
    let mut parts = Vec::new();
    if !before.is_empty() {
        parts.push(ChannelPart::Literal(before.to_owned()));
    }
    if !kept.is_empty() {
        parts.push(ChannelPart::Range {
            start: shown.start as u64,
            end: shown.end as u64,
        });
    }
    if !after.is_empty() {
        parts.push(ChannelPart::Literal(after.to_owned()));
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
        layout: CommandPresentationLayout<'_>,
        stdout: CapturedChannel<'_>,
        stderr: CapturedChannel<'_>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.canonical.is_some(),
            "command presentation requires a canonical live-output writer"
        );
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

    /// Commits any incomplete trailing sequence as the replacement character
    /// whole-stream lossy decoding gives it, so receipts cover the raw text.
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
                    tracing::error!(tool_call_doc_id = %self.tool_call_id, %error,
                        "canonical tool receipt recording failed");
                }
            }
            Err(error) => tracing::error!(tool_call_doc_id = %self.tool_call_id, %error,
                "canonical tool output append failed"),
        }
    }
}

/// Decodes `pending` exactly as `String::from_utf8_lossy` decodes the whole
/// stream: each invalid sequence becomes one U+FFFD, and an incomplete tail
/// stays pending for the next read unless the stream has ended.
fn drain_utf8(pending: &mut Vec<u8>, end_of_stream: bool) -> String {
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

    fn whole(raw: &str) -> CapturedChannel<'_> {
        CapturedChannel {
            raw,
            rendered: raw,
            shown: 0..raw.len(),
        }
    }

    #[derive(Debug, Default)]
    struct MemoryAppender(std::sync::Mutex<String>);

    impl CanonicalOutputAppender for MemoryAppender {
        fn append<'a>(
            &'a self,
            text: &'a str,
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<std::ops::Range<u64>>> + Send + 'a>>
        {
            Box::pin(async move {
                let mut source = self.0.lock().unwrap();
                let start = source.len() as u64;
                source.push_str(text);
                Ok(start..source.len() as u64)
            })
        }
    }

    async fn prepared(
        layout: CommandPresentationLayout<'_>,
        expected: &str,
        stdout: CapturedChannel<'_>,
        stderr: CapturedChannel<'_>,
    ) -> String {
        let registry = LiveToolOutputRegistry::default();
        let appender = Arc::new(MemoryAppender::default());
        let writer = registry
            .canonical_writer_for("tool".into(), appender.clone())
            .await;
        // Interleave the channels so ranges must follow receipts, not order.
        let (out_a, out_b) = stdout.raw.split_at(stdout.raw.len() / 2);
        writer
            .append(LiveOutputStream::Stdout, out_a.as_bytes())
            .await;
        writer
            .append(LiveOutputStream::Stderr, stderr.raw.as_bytes())
            .await;
        writer
            .append(LiveOutputStream::Stdout, out_b.as_bytes())
            .await;
        writer
            .prepare_command_presentation(expected, layout, stdout, stderr)
            .await
            .unwrap();
        let PayloadPresentation::Composed { parts } = registry
            .take_prepared_presentation("tool", expected)
            .await
            .unwrap()
            .unwrap()
        else {
            panic!("command presentation is composed")
        };
        let source = appender.0.lock().unwrap().clone();
        render(&source, &parts)
    }

    #[tokio::test]
    async fn command_layouts_reconstruct_their_exact_text() {
        let stdout = "line \"one\"\nline two\n";
        let stderr = "warn\tthree";
        let metadata = r#"{"ok":true}"#;

        let object = format!(
            r#"{{"ok":true,"stdout":{},"stderr":{}}}"#,
            serde_json::to_string(stdout).unwrap(),
            serde_json::to_string(stderr).unwrap(),
        );
        let rendered = prepared(
            CommandPresentationLayout::JsonObject {
                metadata_json: metadata,
            },
            &object,
            whole(stdout),
            whole(stderr),
        )
        .await;
        assert_eq!(rendered, object);

        let labeled = format!("gents_exec: {metadata}\nstdout:\n{stdout}\nstderr:\n(empty)");
        let rendered = prepared(
            CommandPresentationLayout::Labeled {
                head: &format!("gents_exec: {metadata}\n"),
                json_string: false,
            },
            &labeled,
            whole(stdout),
            CapturedChannel {
                raw: "",
                rendered: "",
                shown: 0..0,
            },
        )
        .await;
        assert_eq!(rendered, labeled);

        let text = format!("exit_code: 0\nstdout:\n{stdout}\nstderr:\n{stderr}");
        let encoded = serde_json::to_string(&text).unwrap();
        let rendered = prepared(
            CommandPresentationLayout::Labeled {
                head: "exit_code: 0\n",
                json_string: true,
            },
            &encoded,
            whole(stdout),
            whole(stderr),
        )
        .await;
        assert_eq!(rendered, encoded);
    }

    #[tokio::test]
    async fn suffix_channels_select_only_the_retained_tail() {
        let stdout = "head that scrolled away\ntail é kept";
        let start = stdout.find("tail").unwrap();
        let shown = format!("[notice]\n\n{}", &stdout[start..]);
        let text = format!("timed out\nstdout:\n{shown}\nstderr:\n(empty)");
        let rendered = prepared(
            CommandPresentationLayout::Labeled {
                head: "timed out\n",
                json_string: false,
            },
            &text,
            CapturedChannel {
                raw: stdout,
                rendered: &shown,
                shown: start..stdout.len(),
            },
            whole(""),
        )
        .await;
        assert_eq!(rendered, text);

        let mismatched = CapturedChannel {
            raw: stdout,
            rendered: "unrelated",
            shown: start..stdout.len(),
        };
        assert!(channel_plan(&mismatched, true).is_err());
        let interior = CapturedChannel {
            raw: stdout,
            rendered: "that",
            shown: 5..9,
        };
        assert!(channel_plan(&interior, true).is_err());
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
            &channel_plan(&whole("éx"), true).unwrap(),
            &stdout,
        )
        .unwrap();
        push_literal(&mut parts, "|".into());
        append_plain_channel(
            &mut parts,
            &channel_plan(&whole("!"), true).unwrap(),
            &stderr,
        )
        .unwrap();
        assert_eq!(render(source, &parts), "éx|!");

        let mut json = Vec::new();
        append_json_channel(
            &mut json,
            "éx",
            &channel_plan(&whole("éx"), false).unwrap(),
            &stdout,
        )
        .unwrap();
        assert_eq!(render(source, &json), "éx");
    }

    #[test]
    fn drained_reads_decode_as_the_whole_stream_does() {
        let stream: &[u8] = b"a\xff\xc3\xa9\xc3\xe2\x82\xacz\xe2\x82";
        for split in 0..=stream.len() {
            for second in split..=stream.len() {
                let mut pending = Vec::new();
                let mut text = String::new();
                for read in [&stream[..split], &stream[split..second], &stream[second..]] {
                    pending.extend_from_slice(read);
                    text.push_str(&drain_utf8(&mut pending, false));
                }
                text.push_str(&drain_utf8(&mut pending, true));
                assert!(pending.is_empty());
                assert_eq!(text, String::from_utf8_lossy(stream), "{split}/{second}");
            }
        }
    }

    #[tokio::test]
    async fn invalid_and_split_bytes_cover_the_lossy_channel() {
        let registry = LiveToolOutputRegistry::default();
        let appender = Arc::new(MemoryAppender::default());
        let writer = registry
            .canonical_writer_for("tool".into(), appender.clone())
            .await;
        // An invalid byte beside a scalar split across reads, then a
        // truncated trailing sequence the process never completes.
        for read in [&b"\xff\xc3"[..], b"\xa9\xe2"] {
            writer.append(LiveOutputStream::Stdout, read).await;
        }
        let raw = String::from_utf8_lossy(b"\xff\xc3\xa9\xe2").into_owned();
        assert_eq!(raw, "\u{FFFD}é\u{FFFD}");
        let text = format!("x\nstdout:\n{raw}\nstderr:\n");
        writer
            .prepare_command_presentation(
                &text,
                CommandPresentationLayout::Labeled {
                    head: "x\n",
                    json_string: false,
                },
                whole(&raw),
                whole(""),
            )
            .await
            .unwrap();
        assert_eq!(*appender.0.lock().unwrap(), raw);
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
