use serde::Serialize;

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
) -> RenderedFileContents {
    let total_lines = text.lines().count();
    let start_line = start_line.unwrap_or(1).max(1);
    let end_line = end_line
        .unwrap_or(total_lines.max(start_line))
        .max(start_line);

    let mut rendered = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let line_number = idx + 1;
        if line_number < start_line || line_number > end_line {
            continue;
        }
        rendered.push(format!("L{line_number}: {line}"));
    }

    let content = if rendered.is_empty() && text.is_empty() {
        "(empty file)".to_string()
    } else if rendered.is_empty() {
        "(no lines in requested range)".to_string()
    } else {
        rendered.join("\n")
    };

    RenderedFileContents {
        content,
        total_lines,
        returned_lines: rendered.len(),
        start_line,
        end_line,
    }
}

pub(crate) fn truncate_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }

    let truncated = text.chars().take(max_chars).collect::<String>();
    format!("{truncated}\n[truncated to {max_chars} chars]")
}
