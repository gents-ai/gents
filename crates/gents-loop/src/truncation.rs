//! Pure text truncation: head/tail line-and-byte clamping with no storage
//! side effect.
//!
//! `gents::truncation` layers a DefraDB-backed spill (`Truncator`,
//! `DefraSpillTruncator`) on top of this module for native tool output that
//! overflows its budget; the loop itself only ever needs the bounded text,
//! never the spill document, so that half stays in `gents`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruncationMode {
    Head,
    Tail,
}

pub fn tool_result_truncation_mode(tool_name: &str) -> TruncationMode {
    match tool_name {
        "bash" | "shell" | "command" => TruncationMode::Tail,
        _ => TruncationMode::Head,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruncationTrigger {
    Lines,
    Bytes,
}

#[derive(Debug, Clone)]
pub struct TruncationLimits {
    pub max_lines: usize,
    pub max_bytes: usize,
}

impl Default for TruncationLimits {
    fn default() -> Self {
        Self {
            max_lines: 2000,
            max_bytes: 50 * 1024,
        }
    }
}

/// Capacity of one live tool-output ring buffer (stdout, stderr, and the
/// combined stream each get their own).
pub const LIVE_STREAM_CAPACITY_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone)]
pub struct TextTruncation {
    pub text: String,
    pub truncated: bool,
    pub trigger: Option<TruncationTrigger>,
    pub original_lines: usize,
    pub original_bytes: usize,
    pub returned_bytes: usize,
}

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

pub fn truncate_text(
    text: &str,
    mode: TruncationMode,
    limits: &TruncationLimits,
) -> (String, Option<TruncationTrigger>, bool) {
    let result = truncate(text, mode, limits);
    (result.text, result.trigger, result.truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_truncation_under_limits() {
        let text = "line 1\nline 2\nline 3";
        let (result, trigger, truncated) =
            truncate_text(text, TruncationMode::Head, &TruncationLimits::default());
        assert!(!truncated);
        assert!(trigger.is_none());
        assert_eq!(result, text);
    }

    #[test]
    fn head_truncation_by_lines() {
        let lines: Vec<String> = (0..100).map(|i| format!("line {}", i)).collect();
        let text = lines.join("\n");
        let limits = TruncationLimits {
            max_lines: 10,
            max_bytes: 1024 * 1024,
        };

        let (result, trigger, truncated) = truncate_text(&text, TruncationMode::Head, &limits);
        assert!(truncated);
        assert_eq!(trigger, Some(TruncationTrigger::Lines));
        assert!(result.starts_with("line 0\n"));
        assert!(result.contains("[Showing lines 1-10 of 100"));
    }

    #[test]
    fn tail_truncation_by_lines() {
        let lines: Vec<String> = (0..100).map(|i| format!("line {}", i)).collect();
        let text = lines.join("\n");
        let limits = TruncationLimits {
            max_lines: 10,
            max_bytes: 1024 * 1024,
        };

        let (result, trigger, truncated) = truncate_text(&text, TruncationMode::Tail, &limits);
        assert!(truncated);
        assert_eq!(trigger, Some(TruncationTrigger::Lines));
        assert!(result.contains("line 99"));
        assert!(result.contains("[Showing lines 91-100 of 100"));
    }

    #[test]
    fn head_truncation_by_bytes() {
        let text = "x".repeat(100_000);
        let limits = TruncationLimits {
            max_lines: 1_000_000,
            max_bytes: 1024,
        };

        let (result, trigger, truncated) = truncate_text(&text, TruncationMode::Head, &limits);
        assert!(truncated);
        assert_eq!(trigger, Some(TruncationTrigger::Bytes));
        assert!(result.len() < 100_000);
    }

    #[test]
    fn tail_truncation_by_bytes() {
        let text = "x".repeat(100_000);
        let limits = TruncationLimits {
            max_lines: 1_000_000,
            max_bytes: 1024,
        };

        let (result, trigger, truncated) = truncate_text(&text, TruncationMode::Tail, &limits);
        assert!(truncated);
        assert_eq!(trigger, Some(TruncationTrigger::Bytes));
        assert!(result.len() < 100_000);
    }

    #[test]
    fn both_limits_exceeded() {
        let lines: Vec<String> = (0..5000).map(|i| format!("line {:04}", i)).collect();
        let text = lines.join("\n");
        let limits = TruncationLimits {
            max_lines: 100,
            max_bytes: 1024,
        };

        let (_, trigger, truncated) = truncate_text(&text, TruncationMode::Head, &limits);
        assert!(truncated);
        assert!(trigger.is_some());
    }
}
