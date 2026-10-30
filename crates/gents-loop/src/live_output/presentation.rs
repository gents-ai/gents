use gents_protocol::output::{PayloadPresentation, PresentationPart};

use super::{
    LiveOutputError, LiveOutputStream, LiveToolOutputRegistry, OutputReceipt,
    PreparedToolPresentation, Result,
};

impl LiveToolOutputRegistry {
    pub(super) async fn prepare_command_presentation(
        &self,
        tool_call_id: &str,
        expected_text: &str,
        layout: CommandPresentationLayout<'_>,
        stdout: CapturedChannel<'_>,
        stderr: CapturedChannel<'_>,
    ) -> Result<()> {
        let mut live = self.inner.lock().await;
        let state = live
            .get_mut(tool_call_id)
            .ok_or(LiveOutputError::PresentationRegistrationMissing)?;
        if channel_len(&state.receipts[LiveOutputStream::Stdout.index()]) != stdout.raw.len() as u64
            || channel_len(&state.receipts[LiveOutputStream::Stderr.index()])
                != stderr.raw.len() as u64
        {
            return Err(LiveOutputError::CaptureReceiptsIncomplete);
        }
        let json_object = matches!(layout, CommandPresentationLayout::JsonObject { .. });
        let stdout_plan = channel_plan(&stdout, !json_object)?;
        let stderr_plan = channel_plan(&stderr, !json_object)?;
        let mut parts = Vec::new();
        match layout {
            CommandPresentationLayout::JsonObject { metadata_json } => {
                if !(metadata_json.starts_with('{') && metadata_json.ends_with('}')) {
                    return Err(LiveOutputError::MetadataNotObject);
                }
                let metadata_fields = &metadata_json[1..metadata_json.len() - 1];
                push_literal(&mut parts, r#"{"stdout":""#.to_owned());
                append_json_channel(&mut parts, stdout.raw, &stdout_plan, &state.receipts[0])?;
                push_literal(&mut parts, r#"","stderr":""#.to_owned());
                append_json_channel(&mut parts, stderr.raw, &stderr_plan, &state.receipts[1])?;
                push_literal(&mut parts, format!("\",{metadata_fields}}}"));
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

#[derive(Debug, Clone, Copy)]
pub enum CommandPresentationLayout<'a> {
    JsonObject { metadata_json: &'a str },
    Labeled { head: &'a str, json_string: bool },
}

#[derive(Debug, Clone)]
pub struct CapturedChannel<'a> {
    pub raw: &'a str,
    pub rendered: &'a str,
    pub shown: std::ops::Range<usize>,
}

#[derive(Clone)]
pub(super) enum ChannelPart {
    Range { start: u64, end: u64 },
    Literal(String),
}

fn channel_len(receipts: &[OutputReceipt]) -> u64 {
    receipts
        .last()
        .map(|receipt| receipt.channel_end)
        .unwrap_or(0)
}

pub(super) fn channel_plan(
    channel: &CapturedChannel<'_>,
    empty_marker: bool,
) -> Result<Vec<ChannelPart>> {
    let CapturedChannel {
        raw,
        rendered,
        shown,
    } = channel;
    if !(shown.start <= shown.end
        && shown.end <= raw.len()
        && raw.is_char_boundary(shown.start)
        && raw.is_char_boundary(shown.end)
        && (shown.start == 0 || shown.end == raw.len()))
    {
        return Err(LiveOutputError::InvalidTruncationRange);
    }
    let kept = &raw[shown.clone()];
    let (before, after) = if shown.start == 0 {
        if !rendered.starts_with(kept) {
            return Err(LiveOutputError::TruncationPrefixMismatch);
        }
        ("", &rendered[kept.len()..])
    } else {
        if !rendered.ends_with(kept) {
            return Err(LiveOutputError::TruncationSuffixMismatch);
        }
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

pub(super) fn push_literal(parts: &mut Vec<PresentationPart>, text: String) {
    if !text.is_empty() {
        parts.push(PresentationPart::Literal { text });
    }
}

pub(super) fn append_plain_channel(
    output: &mut Vec<PresentationPart>,
    plan: &[ChannelPart],
    receipts: &[OutputReceipt],
) -> Result<()> {
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
) -> Result<()> {
    let mut cursor = start;
    for receipt in receipts {
        let from = cursor.max(receipt.channel_start);
        let until = end.min(receipt.channel_end);
        if from >= until {
            continue;
        }
        let source_start = receipt.source_start + (from - receipt.channel_start);
        let source_end = receipt.source_start + (until - receipt.channel_start);
        if source_end > receipt.source_end {
            return Err(LiveOutputError::ReceiptRangeExceedsChunk);
        }
        output.push(PresentationPart::OutputRange {
            start_byte: source_start,
            end_byte: source_end,
        });
        cursor = until;
    }
    if cursor != end {
        return Err(LiveOutputError::ReceiptGap);
    }
    Ok(())
}

pub(super) fn append_json_channel(
    output: &mut Vec<PresentationPart>,
    raw: &str,
    plan: &[ChannelPart],
    receipts: &[OutputReceipt],
) -> Result<()> {
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

fn json_fragment(text: &str) -> Result<String> {
    let encoded = serde_json::to_string(text)?;
    Ok(encoded[1..encoded.len() - 1].to_owned())
}
