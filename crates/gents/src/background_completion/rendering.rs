/// Identifies the complete runtime wrapper for display accounting, not authorization.
pub(crate) fn is_tool_completion_text(text: &str) -> bool {
    text.starts_with("<tool-completion tool_call_id=\"") && text.ends_with("\n</tool-completion>")
}

pub(crate) const NOTIFICATION_SUMMARY_BYTES: usize = 4000;

pub(crate) fn tool_completion_presentation(
    tool_call_id: &str,
    tool_name: &str,
    status: &str,
    result: &str,
    reason: Option<&str>,
    budget: usize,
) -> (String, Vec<gents_protocol::output::PresentationPart>) {
    use gents_protocol::output::PresentationPart;

    let prefix = format!(
        "<tool-completion tool_call_id=\"{}\" tool_name=\"{}\" status=\"{}\">\n  <result>",
        xml_escape_attr(tool_call_id),
        xml_escape_attr(tool_name),
        xml_escape_attr(status),
    );
    let reason_element = reason
        .map(|reason| format!("\n  <reason>{}</reason>", xml_escape_text(reason)))
        .unwrap_or_default();
    let suffix = format!("</result>{reason_element}\n</tool-completion>");
    let mut parts = vec![PresentationPart::Literal {
        text: prefix.clone(),
    }];
    let mut rendered_result = String::new();
    let mut normalized_bytes = 0usize;
    let mut saw_text = false;
    let mut pending_space = false;
    let mut truncated = false;
    let mut range_start = None::<usize>;
    let mut range_end = 0usize;
    let flush_range = |parts: &mut Vec<PresentationPart>, start: &mut Option<usize>, end: usize| {
        if let Some(start) = start.take() {
            parts.push(PresentationPart::OutputRange {
                start_byte: start as u64,
                end_byte: end as u64,
            });
        }
    };
    for (index, ch) in result.char_indices() {
        if ch.is_whitespace() {
            pending_space |= saw_text;
            continue;
        }
        let width = ch.len_utf8();
        let separator = usize::from(pending_space && saw_text);
        if normalized_bytes + separator + width > budget {
            truncated = true;
            break;
        }
        if separator == 1 {
            flush_range(&mut parts, &mut range_start, range_end);
            rendered_result.push(' ');
            parts.push(PresentationPart::Literal { text: " ".into() });
            normalized_bytes += 1;
        }
        pending_space = false;
        saw_text = true;
        normalized_bytes += width;
        match ch {
            '&' | '<' | '>' | '"' | '\'' => {
                flush_range(&mut parts, &mut range_start, range_end);
                let escaped = xml_escape_text(&ch.to_string());
                rendered_result.push_str(&escaped);
                parts.push(PresentationPart::Literal { text: escaped });
            }
            _ => {
                rendered_result.push(ch);
                if range_start.is_none() {
                    range_start = Some(index);
                }
                range_end = index + width;
            }
        }
    }
    flush_range(&mut parts, &mut range_start, range_end);
    if truncated {
        rendered_result.push_str("...");
        parts.push(PresentationPart::Literal { text: "...".into() });
    }
    parts.push(PresentationPart::Literal {
        text: suffix.clone(),
    });
    (format!("{prefix}{rendered_result}{suffix}"), parts)
}

pub(super) fn xml_escape_attr(value: &str) -> String {
    xml_escape_text(value)
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub(super) fn xml_escape_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod context_accounting_tests {
    use super::*;

    #[test]
    fn tool_completion_text_matches_the_rendered_notification() {
        let (text, _) =
            tool_completion_presentation("call", "shell", "complete", "done", None, 4000);
        assert!(is_tool_completion_text(&text));
        assert!(!is_tool_completion_text(&format!("User quotation: {text}")));
    }
}
