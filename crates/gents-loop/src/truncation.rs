//! Pure text truncation: head/tail line-and-byte clamping with no storage
//! side effect. The full output is retained by the canonical transcript;
//! this only selects what the model is shown.

/// Every notice `truncate` writes starts with one of these. Compaction
/// recognizes already-truncated tool output by them, so a new notice shape
/// must be added here.
pub const TRUNCATION_NOTICE_PREFIXES: [&str; 4] = [
    "[Showing lines ",
    "[Showing first ",
    "[Showing last ",
    "[Output omitted: ",
];

/// The notice heading a byte-exact tail of `total` bytes.
pub fn tail_bytes_notice(shown: usize, total: usize) -> String {
    format!("[Showing last {shown} of {total} bytes]\n\n")
}

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

/// Byte ranges of the lines `str::lines` yields, each keeping any `\r`
/// before its `\n`.
fn line_spans(text: &str) -> impl Iterator<Item = std::ops::Range<usize>> + '_ {
    let mut start = 0;
    text.split_inclusive('\n').map(move |line| {
        let range = start..start + line.strip_suffix('\n').unwrap_or(line).len();
        start += line.len();
        range
    })
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

    if limits.max_bytes == 0 {
        return TextTruncation {
            text: format!(
                "[Output omitted: byte limit is zero ({} bytes total)]",
                original_bytes
            ),
            truncated: true,
            trigger: Some(trigger),
            original_lines,
            original_bytes,
            returned_bytes: 0,
        };
    }

    let (truncated, returned_bytes) = match mode {
        TruncationMode::Head => {
            // The shown text is a byte prefix of `text`, so a line keeps its
            // `\r` before `\n`: composed presentations select that prefix
            // from the committed output and must reproduce it exactly.
            let mut shown = 0;
            let mut line_count = 0;
            let mut partial = None;

            for line in line_spans(text) {
                if line_count >= limits.max_lines {
                    break;
                }
                let line_text = &text[line.clone()];
                let separator = usize::from(line_count > 0);
                if shown + line_text.len() + 1 > limits.max_bytes {
                    let available = limits
                        .max_bytes
                        .saturating_sub(shown + separator)
                        .min(line_text.len());
                    let end = floor_char_boundary(line_text, available);
                    if line_count > 0 && end > 0 && line_text.len() > limits.max_bytes {
                        shown = line.start + end;
                        partial = Some((end, line_text.len()));
                    }
                    break;
                }
                shown = line.end;
                line_count += 1;
            }
            let result = &text[..shown];

            if line_count == 0 && exceeds_bytes && limits.max_lines > 0 {
                let end = floor_char_boundary(text, limits.max_bytes.min(original_bytes));
                let result = &text[..end];
                return TextTruncation {
                    text: format!(
                        "{}\n\n[Showing first {} of {} bytes]",
                        result, end, original_bytes,
                    ),
                    truncated: true,
                    trigger: Some(trigger),
                    original_lines,
                    original_bytes,
                    returned_bytes: end,
                };
            }

            let returned_bytes = result.len();
            let notice = match partial {
                Some((shown, line_bytes)) => format!(
                    "[Showing lines 1-{} and the first {} of {} bytes of line {} of {} ({} bytes total)]",
                    line_count,
                    shown,
                    line_bytes,
                    line_count + 1,
                    original_lines,
                    original_bytes,
                ),
                None => format!(
                    "[Showing lines 1-{} of {} ({} bytes total)]",
                    line_count, original_lines, original_bytes,
                ),
            };
            (format!("{}\n\n{}", result, notice), returned_bytes)
        }
        TruncationMode::Tail => {
            let start_line = if exceeds_lines {
                original_lines.saturating_sub(limits.max_lines)
            } else {
                0
            };

            let mut result = String::new();
            let mut included = 0;
            let mut partial = None;

            for line in lines[start_line..].iter().rev() {
                let separator = usize::from(included > 0);
                if result.len() + line.len() + 1 > limits.max_bytes {
                    let available = limits
                        .max_bytes
                        .saturating_sub(result.len() + separator)
                        .min(line.len());
                    let start = ceil_char_boundary(line, line.len() - available);
                    if included > 0 && start < line.len() && line.len() > limits.max_bytes {
                        result = format!("{}\n{}", &line[start..], result);
                        partial = Some((line.len() - start, line.len()));
                    }
                    break;
                }
                if included == 0 {
                    result = line.to_string();
                } else {
                    result = format!("{}\n{}", line, result);
                }
                included += 1;
            }

            if included == 0 && exceeds_bytes && limits.max_lines > 0 {
                let start =
                    ceil_char_boundary(text, original_bytes.saturating_sub(limits.max_bytes));
                let result = &text[start..];
                return TextTruncation {
                    text: format!(
                        "{}{result}",
                        tail_bytes_notice(original_bytes - start, original_bytes)
                    ),
                    truncated: true,
                    trigger: Some(trigger),
                    original_lines,
                    original_bytes,
                    returned_bytes: original_bytes - start,
                };
            }

            let returned_bytes = result.len();
            let shown_start = original_lines - included + 1;
            let notice = match partial {
                Some((shown, line_bytes)) => format!(
                    "[Showing last {} of {} bytes of line {} and lines {}-{} of {} ({} bytes total)]",
                    shown,
                    line_bytes,
                    shown_start - 1,
                    shown_start,
                    original_lines,
                    original_lines,
                    original_bytes,
                ),
                None => format!(
                    "[Showing lines {}-{} of {} ({} bytes total)]",
                    shown_start, original_lines, original_lines, original_bytes,
                ),
            };
            (format!("{}\n\n{}", notice, result), returned_bytes)
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

/// The largest UTF-8 boundary of `text` at or below `index`.
pub fn floor_char_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// The smallest UTF-8 boundary of `text` at or above `index`.
pub fn ceil_char_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index += 1;
    }
    index
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
        assert!(result.starts_with(&"x".repeat(1024)));
        assert!(result.contains("[Showing first 1024 of 100000 bytes]"));
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
        assert!(result.ends_with(&"x".repeat(1024)));
        assert!(result.contains("[Showing last 1024 of 100000 bytes]"));
    }

    #[test]
    fn every_truncated_output_carries_a_notice_prefix() {
        let many_lines = (0..50)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let one_line = "x".repeat(200);
        let cases = [
            (
                many_lines.as_str(),
                TruncationLimits {
                    max_lines: 5,
                    max_bytes: 10_000,
                },
            ),
            (
                many_lines.as_str(),
                TruncationLimits {
                    max_lines: 100,
                    max_bytes: 40,
                },
            ),
            (
                one_line.as_str(),
                TruncationLimits {
                    max_lines: 10,
                    max_bytes: 50,
                },
            ),
            (
                one_line.as_str(),
                TruncationLimits {
                    max_lines: 10,
                    max_bytes: 0,
                },
            ),
        ];
        for (text, limits) in &cases {
            for mode in [TruncationMode::Head, TruncationMode::Tail] {
                let result = truncate(text, mode, limits);
                assert!(result.truncated);
                assert!(
                    TRUNCATION_NOTICE_PREFIXES
                        .iter()
                        .any(|prefix| result.text.contains(prefix)),
                    "{mode:?} {limits:?}: {}",
                    result.text
                );
            }
        }
    }

    #[test]
    fn an_empty_line_that_fits_keeps_the_line_budget() {
        // The empty line is a whole line; the byte fallback must not add a
        // second one past max_lines.
        let limits = TruncationLimits {
            max_lines: 1,
            max_bytes: 3,
        };
        let head = truncate("\nabcdefgh", TruncationMode::Head, &limits);
        assert!(
            head.text.starts_with("\n\n[Showing lines 1-1 of 2"),
            "{:?}",
            head.text
        );
        assert_eq!(head.returned_bytes, 0);

        let tail = truncate("abcdefgh\n\n", TruncationMode::Tail, &limits);
        assert!(
            tail.text.starts_with("[Showing lines 2-2 of 2"),
            "{:?}",
            tail.text
        );
        assert_eq!(tail.returned_bytes, 0);
    }

    #[test]
    fn oversized_utf8_line_preserves_char_boundaries() {
        let text = "é".repeat(10);
        let limits = TruncationLimits {
            max_lines: 10,
            max_bytes: 5,
        };

        let head = truncate(&text, TruncationMode::Head, &limits);
        assert_eq!(head.returned_bytes, 4);
        assert!(head.text.starts_with("éé\n\n[Showing first 4 of 20 bytes]"));

        let tail = truncate(&text, TruncationMode::Tail, &limits);
        assert_eq!(tail.returned_bytes, 4);
        assert!(tail.text.ends_with("[Showing last 4 of 20 bytes]\n\néé"));
    }

    fn results_then_one_large_json_line() -> String {
        let payload = format!("{{\"items\":[{}]}}", vec!["\"v\""; 20_000].join(","));
        format!("Results:\n{payload}")
    }

    #[test]
    fn short_line_before_oversized_line_keeps_the_payload_head() {
        let text = results_then_one_large_json_line();
        let limits = TruncationLimits::default();
        let head = truncate(&text, TruncationMode::Head, &limits);
        let payload_bytes = text.len() - "Results:\n".len();
        let shown = limits.max_bytes - "Results:\n".len();
        assert!(head.truncated);
        assert_eq!(head.returned_bytes, limits.max_bytes);
        assert!(text.starts_with(&head.text[..head.returned_bytes]));
        assert!(head.text.starts_with("Results:\n{\"items\":[\"v\""));
        assert!(
            head.text.ends_with(&format!(
                "\n\n[Showing lines 1-1 and the first {shown} of {payload_bytes} bytes of line 2 of 2 ({} bytes total)]",
                text.len()
            )),
            "{}",
            &head.text[head.returned_bytes..]
        );
    }

    #[test]
    fn oversized_line_before_short_line_keeps_the_payload_tail() {
        let payload = format!("{{\"items\":[{}]}}", vec!["\"v\""; 20_000].join(","));
        let text = format!("{payload}\nexit status 1");
        let limits = TruncationLimits::default();
        let tail = truncate(&text, TruncationMode::Tail, &limits);
        let shown = limits.max_bytes - "\nexit status 1".len();
        assert!(tail.truncated);
        assert_eq!(tail.returned_bytes, limits.max_bytes);
        assert!(text.ends_with(&tail.text[tail.text.len() - tail.returned_bytes..]));
        assert!(tail.text.ends_with("\"v\"]}\nexit status 1"));
        assert!(
            tail.text.starts_with(&format!(
                "[Showing last {shown} of {} bytes of line 1 and lines 2-2 of 2 ({} bytes total)]\n\n",
                payload.len(),
                text.len()
            )),
            "{}",
            &tail.text[..200]
        );
    }

    #[test]
    fn partial_crossing_line_respects_utf8_boundaries() {
        let limits = TruncationLimits {
            max_lines: 10,
            max_bytes: 8,
        };
        // "ok\n" is 3 bytes; 5 remain, which splits the third "é".
        let head = truncate(
            &format!("ok\n{}", "é".repeat(10)),
            TruncationMode::Head,
            &limits,
        );
        assert_eq!(head.returned_bytes, 7);
        assert!(head.text.starts_with(
            "ok\néé\n\n[Showing lines 1-1 and the first 4 of 20 bytes of line 2 of 2"
        ));

        let tail = truncate(
            &format!("{}\nok", "é".repeat(10)),
            TruncationMode::Tail,
            &limits,
        );
        assert_eq!(tail.returned_bytes, 7);
        assert!(tail
            .text
            .starts_with("[Showing last 4 of 20 bytes of line 1 and lines 2-2 of 2"));
        assert!(tail.text.ends_with("\n\néé\nok"));
    }

    #[test]
    fn exhausted_line_budget_does_not_add_a_partial_line() {
        let limits = TruncationLimits {
            max_lines: 1,
            max_bytes: 10,
        };
        let head = truncate("ok\nabcdefghijklmnop", TruncationMode::Head, &limits);
        assert!(
            head.text.starts_with("ok\n\n[Showing lines 1-1 of 2"),
            "{}",
            head.text
        );
        assert_eq!(head.returned_bytes, 2);
    }

    #[test]
    fn empty_leading_line_keeps_a_contiguous_prefix() {
        let limits = TruncationLimits {
            max_lines: 10,
            max_bytes: 5,
        };
        let text = "\nabcdefgh";
        let head = truncate(text, TruncationMode::Head, &limits);
        assert!(text.starts_with(&head.text[..head.returned_bytes]));
        assert_eq!(&head.text[..head.returned_bytes], "\nabcd");
    }

    #[test]
    fn partial_line_notices_carry_a_notice_prefix() {
        let text = results_then_one_large_json_line();
        for mode in [TruncationMode::Head, TruncationMode::Tail] {
            let result = truncate(&text, mode, &TruncationLimits::default());
            assert!(TRUNCATION_NOTICE_PREFIXES
                .iter()
                .any(|prefix| result.text.contains(prefix)));
        }
    }

    #[test]
    fn a_crossing_line_that_fits_the_budget_is_not_cut() {
        let limits = TruncationLimits {
            max_lines: 10,
            max_bytes: 12,
        };
        for mode in [TruncationMode::Head, TruncationMode::Tail] {
            let result = truncate("L1: alpha\nL2: beta\nL3: gamma", mode, &limits);
            assert!(
                !result.text.contains("bytes of line"),
                "{mode:?}: {}",
                result.text
            );
        }
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

    #[test]
    fn head_truncation_shows_an_exact_prefix_of_crlf_text() {
        let text = "one\r\ntwo\r\nthree\r\n".repeat(20);
        for max_bytes in [8, 12, 40, 100] {
            let result = truncate(
                &text,
                TruncationMode::Head,
                &TruncationLimits {
                    max_bytes,
                    max_lines: usize::MAX,
                },
            );
            assert!(result.truncated);
            let shown = &text[..result.returned_bytes];
            assert!(result.returned_bytes <= max_bytes, "{max_bytes}");
            assert!(
                result.text.starts_with(shown),
                "{max_bytes}: {:?}",
                result.text
            );
        }
    }
}
