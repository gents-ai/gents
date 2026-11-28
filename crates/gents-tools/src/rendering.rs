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

/// A command result's streams, from the body after its `gents_exec:` line.
pub struct CommandStreams<'a> {
    pub stdout: &'a str,
    pub stderr: &'a str,
}

/// Splits a command result body into its streams. The metadata's byte counts
/// place the stderr marker exactly while a stream is untruncated, since either
/// stream may itself contain the marker's text; otherwise the first marker
/// splits them. A stream written as `(empty)` reads as empty, and a body
/// without the stdout header is all stdout.
pub fn command_streams<'a>(
    body: &'a str,
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> CommandStreams<'a> {
    const STDOUT_PREFIX: &str = "stdout:\n";
    const STDERR_MARKER: &str = "\nstderr:\n";
    const EMPTY: &str = "(empty)";
    let Some(rest) = body.strip_prefix(STDOUT_PREFIX) else {
        return CommandStreams {
            stdout: body,
            stderr: "",
        };
    };
    let written = |key: &str| {
        let counts = meta?.get(key)?.as_object()?;
        let truncated = counts.get("truncated").and_then(serde_json::Value::as_bool);
        let bytes = usize::try_from(counts.get("returned_bytes")?.as_u64()?).ok()?;
        (truncated != Some(true)).then_some(if bytes == 0 { EMPTY.len() } else { bytes })
    };
    let marker_at = |at: usize| rest.get(at..)?.starts_with(STDERR_MARKER).then_some(at);
    let from_end = |bytes: usize| rest.len().checked_sub(bytes + STDERR_MARKER.len());
    let at = written("stdout_truncation")
        .and_then(marker_at)
        .or_else(|| {
            written("stderr_truncation")
                .and_then(from_end)
                .and_then(marker_at)
        })
        .or_else(|| rest.find(STDERR_MARKER));
    let (stdout, stderr) = match at {
        Some(at) => (&rest[..at], &rest[at + STDERR_MARKER.len()..]),
        None => (rest, ""),
    };
    let stream = |text: &'a str| if text == EMPTY { "" } else { text };
    CommandStreams {
        stdout: stream(stdout),
        stderr: stream(stderr),
    }
}

#[cfg(test)]
mod command_streams_tests {
    use super::*;

    fn split(body: &str, meta: serde_json::Value) -> (&str, &str) {
        let streams = command_streams(body, meta.as_object());
        (streams.stdout, streams.stderr)
    }

    #[test]
    fn command_streams_split_at_the_counted_marker() {
        let body = "stdout:\na\nstderr:\nb\nstderr:\nc";
        let counted = serde_json::json!({ "stdout_truncation": { "returned_bytes": 11 } });
        assert_eq!(split(body, counted), ("a\nstderr:\nb", "c"));
        assert_eq!(split(body, serde_json::json!({})), ("a", "b\nstderr:\nc"));
        let empty = "stdout:\n(empty)\nstderr:\nboom";
        assert_eq!(split(empty, serde_json::json!({})), ("", "boom"));
        assert_eq!(
            split("plain text", serde_json::json!({})),
            ("plain text", "")
        );
    }
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
