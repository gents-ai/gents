/// Identifies the complete runtime wrapper for display accounting, not authorization.
pub(crate) fn is_tool_completion_text(text: &str) -> bool {
    text.starts_with("<tool-completion tool_call_id=\"") && text.ends_with("\n</tool-completion>")
}

pub const NOTIFICATION_SUMMARY_BYTES: usize = 4000;

pub fn tool_completion_presentation(
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionMetadata {
    pub tool_call_id: String,
    pub tool_name: String,
    pub status: String,
    pub reason: Option<String>,
    pub result_start_byte: u64,
    pub result_end_byte: u64,
}

pub fn completion_metadata(
    parts: &[gents_protocol::output::PresentationPart],
) -> Option<CompletionMetadata> {
    use gents_protocol::output::PresentationPart;
    let PresentationPart::Literal { text: opening } = parts.first()? else {
        return None;
    };
    let PresentationPart::Literal { text: closing } = parts.last()? else {
        return None;
    };
    let fields = opening.strip_prefix("<tool-completion tool_call_id=\"")?;
    let (call, fields) = fields.split_once("\" tool_name=\"")?;
    let (name, fields) = fields.split_once("\" status=\"")?;
    let status = fields.strip_suffix("\">\n  <result>")?;
    let reason = closing
        .strip_prefix("</result>")?
        .strip_suffix("\n</tool-completion>")?;
    let reason = if reason.is_empty() {
        None
    } else {
        Some(unescape_xml(
            reason
                .strip_prefix("\n  <reason>")?
                .strip_suffix("</reason>")?,
        ))
    };
    let size = parts.iter().try_fold(0u64, |size, part| {
        let bytes = match part {
            PresentationPart::Literal { text } => u64::try_from(text.len()).ok()?,
            PresentationPart::OutputRange {
                start_byte,
                end_byte,
            } => end_byte.checked_sub(*start_byte)?,
        };
        size.checked_add(bytes)
    })?;
    Some(CompletionMetadata {
        tool_call_id: unescape_xml(call),
        tool_name: unescape_xml(name),
        status: unescape_xml(status),
        reason,
        result_start_byte: opening.len().try_into().ok()?,
        result_end_byte: size.checked_sub(closing.len().try_into().ok()?)?,
    })
}

fn unescape_xml(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod context_accounting_tests {
    use super::*;

    #[test]
    fn completion_metadata_roundtrips_the_owned_wire_format() {
        for (result, budget, expected) in [
            ("", 4000, ""),
            ("  café\n<&>  done", 4000, "café &lt;&amp;&gt; done"),
            ("雪abc", 3, "雪..."),
        ] {
            let (text, parts) = tool_completion_presentation(
                "call\"&lt;",
                "shell'&",
                "failed",
                result,
                Some("tool<&> failed"),
                budget,
            );
            let metadata = completion_metadata(&parts).expect("owned completion framing");
            assert_eq!(metadata.tool_call_id, "call\"&lt;");
            assert_eq!(metadata.tool_name, "shell'&");
            assert_eq!(metadata.status, "failed");
            assert_eq!(metadata.reason.as_deref(), Some("tool<&> failed"));
            assert_eq!(
                &text[metadata.result_start_byte as usize..metadata.result_end_byte as usize],
                expected,
            );
        }
        let (_, parts) =
            tool_completion_presentation("call", "shell", "success", "done", None, 4000);
        assert_eq!(
            completion_metadata(&parts).expect("owned framing").reason,
            None
        );
    }

    #[test]
    fn tool_completion_text_matches_the_rendered_notification() {
        let (text, _) =
            tool_completion_presentation("call", "shell", "complete", "done", None, 4000);
        assert!(is_tool_completion_text(&text));
        assert!(!is_tool_completion_text(&format!("User quotation: {text}")));
    }
}
