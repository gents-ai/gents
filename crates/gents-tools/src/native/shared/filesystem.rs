use serde::Serialize;

use gents_loop::truncation::{truncate, TruncationLimits, TruncationMode};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RenderedFileContents {
    pub content: String,
    pub total_lines: usize,
    pub returned_lines: usize,
    pub start_line: usize,
    pub end_line: usize,
}

pub(crate) fn render_file_contents(
    text: &str,
    start_line: Option<usize>,
    end_line: Option<usize>,
    numbered: bool,
) -> RenderedFileContents {
    let total_lines = text.lines().count();
    let start_line = start_line.unwrap_or(1).max(1);
    let end_line = end_line
        .unwrap_or(total_lines.max(start_line))
        .max(start_line);
    let selected = |index: &usize| (start_line..=end_line).contains(&(index + 1));

    let (content, returned_lines) = if numbered {
        let rendered: Vec<String> = text
            .lines()
            .enumerate()
            .filter(|(index, _)| selected(index))
            .map(|(index, line)| format!("L{}: {line}", index + 1))
            .collect();
        let content = if rendered.is_empty() && text.is_empty() {
            "(empty file)".to_string()
        } else if rendered.is_empty() {
            "(no lines in requested range)".to_string()
        } else {
            rendered.join("\n")
        };
        (content, rendered.len())
    } else {
        let lines: Vec<&str> = text
            .split_inclusive('\n')
            .enumerate()
            .filter(|(index, _)| selected(index))
            .map(|(_, line)| line)
            .collect();
        (lines.concat(), lines.len())
    };

    RenderedFileContents {
        content,
        total_lines,
        returned_lines,
        start_line,
        end_line,
    }
}

pub(crate) fn cap_output(text: &str, max_bytes: usize) -> (String, bool) {
    let limits = TruncationLimits {
        max_bytes,
        max_lines: usize::MAX,
    };
    let result = truncate(text, TruncationMode::Head, &limits);
    (result.text, result.truncated)
}
