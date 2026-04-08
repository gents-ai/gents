use crate::truncation::{TruncationLimits, TruncationMode, TruncationTrigger};

pub fn truncate_text(
    text: &str,
    mode: TruncationMode,
    limits: &TruncationLimits,
) -> (String, Option<TruncationTrigger>, bool) {
    let original_bytes = text.len();
    let lines: Vec<&str> = text.lines().collect();
    let original_lines = lines.len();

    let exceeds_lines = original_lines > limits.max_lines;
    let exceeds_bytes = original_bytes > limits.max_bytes;

    if !exceeds_lines && !exceeds_bytes {
        return (text.to_string(), None, false);
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

    let truncated = match mode {
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

            format!(
                "{}\n\n[Showing lines 1-{} of {} ({} bytes total)]",
                result, line_count, original_lines, original_bytes,
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

            let shown_start = original_lines - included + 1;
            format!(
                "[Showing lines {}-{} of {} ({} bytes total)]\n\n{}",
                shown_start, original_lines, original_lines, original_bytes, result,
            )
        }
    };

    (truncated, Some(trigger), true)
}
