//! Model-facing output-evidence wrapper over the shared wait-status
//! projection in `gents-protocol`. The projection itself (lifecycle
//! certainty, elapsed-time truth, skew handling) lives in
//! `gents_protocol::wait_status` so the desktop bridge consumes the exact
//! same pure contract without duplicating logic.

pub use gents_protocol::wait_status::{WaitStatusProjection, PREVIEW_MAX_BYTES};

/// Bounded recent-output evidence derived from the canonical ToolOutput
/// text. Byte volume is always truthful; text is exposed only as a trailing
/// preview so a caller that wants more must page through read_process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputEvidence {
    pub total_bytes: u64,
    pub preview: Option<String>,
    pub preview_truncated: bool,
}

impl OutputEvidence {
    pub fn from_text(text: &str) -> Self {
        let total_bytes = text.len() as u64;
        if text.is_empty() {
            return Self {
                total_bytes,
                preview: None,
                preview_truncated: false,
            };
        }
        let (preview, truncated) = bounded_tail(text, PREVIEW_MAX_BYTES);
        Self {
            total_bytes,
            preview: Some(preview),
            preview_truncated: truncated,
        }
    }
}

/// Tail-bounded slice: keeps the most recent output, snapped to a char
/// boundary, reporting whether bytes were dropped from the front.
fn bounded_tail(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_owned(), false);
    }
    let mut start = text.len() - max_bytes;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    (text[start..].to_owned(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_evidence_bounds_preview_and_reports_volume() {
        let text = "x".repeat(PREVIEW_MAX_BYTES + 10);
        let evidence = OutputEvidence::from_text(&text);
        assert_eq!(evidence.total_bytes, (PREVIEW_MAX_BYTES + 10) as u64);
        assert_eq!(
            evidence.preview.as_deref().map(str::len),
            Some(PREVIEW_MAX_BYTES)
        );
        assert!(evidence.preview_truncated);

        let short = OutputEvidence::from_text("hello");
        assert_eq!(short.total_bytes, 5);
        assert_eq!(short.preview.as_deref(), Some("hello"));
        assert!(!short.preview_truncated);

        let empty = OutputEvidence::from_text("");
        assert_eq!(empty.total_bytes, 0);
        assert_eq!(empty.preview, None);
    }

    #[test]
    fn output_evidence_preview_snaps_to_char_boundary() {
        let text = "界".repeat(700);
        let evidence = OutputEvidence::from_text(&text);
        let preview = evidence.preview.expect("multi-byte tail preserved");
        assert!(preview.chars().all(|c| c == '界'));
        assert!(preview.len() <= PREVIEW_MAX_BYTES + 3);
        assert_eq!(preview.chars().count() * 3, preview.len());
        assert!(evidence.preview_truncated);
    }
}
