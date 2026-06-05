use crate::truncation::{TruncationLimits, TruncationMode, TruncationTrigger};

/// Result of the canonical honest truncation. Carries enough structured
/// metadata that callers needing a machine-readable envelope (e.g. the bash
/// stdout/stderr truncation summary) can build it without re-counting bytes.
#[derive(Debug, Clone)]
pub struct TextTruncation {
    /// The (possibly truncated) text, with an honest marker appended/prepended
    /// when truncation occurred.
    pub text: String,
    /// Whether truncation occurred.
    pub truncated: bool,
    /// Which limit forced truncation, if any.
    pub trigger: Option<TruncationTrigger>,
    /// Total lines in the original input.
    pub original_lines: usize,
    /// Total bytes in the original input.
    pub original_bytes: usize,
    /// Bytes of original content retained (excludes the honest marker).
    pub returned_bytes: usize,
}

/// Honest line+byte truncator. This is the single truncation primitive for
/// model-ingested tool output across the runtime: it bounds by both line count
/// and byte size and always appends/prepends a marker stating exactly what was
/// shown vs. what existed (`[Showing lines 1-N of M (B bytes total)]`). Callers
/// that only need the text+flags should use the [`truncate_text`] wrapper.
pub fn truncate(text: &str, mode: TruncationMode, limits: &TruncationLimits) -> TextTruncation {
    let original_bytes = text.len();
    let lines: Vec<&str> = text.lines().collect();
    let original_lines = lines.len();

    let exceeds_lines = original_lines > limits.max_lines;
    let exceeds_bytes = original_bytes > limits.max_bytes;

    if !exceeds_lines && !exceeds_bytes {
        return TextTruncation {
            text: text.to_string(),
            truncated: false,
            trigger: None,
            original_lines,
            original_bytes,
            returned_bytes: original_bytes,
        };
    }

    let trigger = if exceeds_bytes && exceeds_lines {
        let line_ratio = original_lines as f64 / limits.max_lines as f64;
        let byte_ratio = original_bytes as f64 / limits.max_bytes as f64;
        if byte_ratio > line_ratio {
            TruncationTrigger::Bytes
        } else {
            TruncationTrigger::Lines
        }
    } else if exceeds_bytes {
        TruncationTrigger::Bytes
    } else {
        TruncationTrigger::Lines
    };

    let (truncated, returned_bytes) = match mode {
        TruncationMode::Head => {
            let mut result = String::new();
            let mut line_count = 0;

            for line in &lines {
                if line_count >= limits.max_lines {
                    break;
                }
                if result.len() + line.len() + 1 > limits.max_bytes {
                    break;
                }
                if !result.is_empty() {
                    result.push('\n');
                }
                result.push_str(line);
                line_count += 1;
            }

            let returned_bytes = result.len();
            (
                format!(
                    "{}\n\n[Showing lines 1-{} of {} ({} bytes total)]",
                    result, line_count, original_lines, original_bytes,
                ),
                returned_bytes,
            )
        }
        TruncationMode::Tail => {
            let start_line = if exceeds_lines {
                original_lines.saturating_sub(limits.max_lines)
            } else {
                0
            };

            let mut result = String::new();
            let mut included = 0;

            for line in lines[start_line..].iter().rev() {
                if result.len() + line.len() + 1 > limits.max_bytes {
                    break;
                }
                included += 1;
                if result.is_empty() {
                    result = line.to_string();
                } else {
                    result = format!("{}\n{}", line, result);
                }
            }

            let returned_bytes = result.len();
            let shown_start = original_lines - included + 1;
            (
                format!(
                    "[Showing lines {}-{} of {} ({} bytes total)]\n\n{}",
                    shown_start, original_lines, original_lines, original_bytes, result,
                ),
                returned_bytes,
            )
        }
    };

    TextTruncation {
        text: truncated,
        truncated: true,
        trigger: Some(trigger),
        original_lines,
        original_bytes,
        returned_bytes,
    }
}

/// Tuple-returning convenience wrapper over [`truncate`] for callers that only
/// need `(text, trigger, truncated)`.
pub fn truncate_text(
    text: &str,
    mode: TruncationMode,
    limits: &TruncationLimits,
) -> (String, Option<TruncationTrigger>, bool) {
    let result = truncate(text, mode, limits);
    (result.text, result.trigger, result.truncated)
}
