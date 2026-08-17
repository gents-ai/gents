#[derive(Debug, Clone, Default)]
pub struct PreviewSignatureInput {
    pub root_request_id: String,
    pub root_state: Option<String>,
    pub root_interrupt_requested_at: Option<String>,
    pub affected: Vec<PreviewSignatureRow>,
}

#[derive(Debug, Clone, Default)]
pub struct PreviewSignatureRow {
    pub request_id: String,
    pub lifecycle_state: Option<String>,
    pub await_mode: Option<String>,
    pub cancel_policy: Option<String>,
    pub parent_tool_call_id: Option<String>,
}

pub fn compute_preview_signature(input: &PreviewSignatureInput) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(input.root_request_id.as_bytes());
    hasher.update(&[0x1F]);
    hasher.update(input.root_state.as_deref().unwrap_or("").as_bytes());
    hasher.update(&[0x1F]);
    hasher.update(
        input
            .root_interrupt_requested_at
            .as_deref()
            .unwrap_or("")
            .as_bytes(),
    );
    hasher.update(&[0x1E]);

    let mut sorted: Vec<&PreviewSignatureRow> = input.affected.iter().collect();
    sorted.sort_by(|a, b| a.request_id.cmp(&b.request_id));
    for (idx, row) in sorted.iter().enumerate() {
        if idx > 0 {
            hasher.update(&[0x1F]);
        }
        hasher.update(row.request_id.as_bytes());
        hasher.update(&[0x1D]);
        hasher.update(row.lifecycle_state.as_deref().unwrap_or("").as_bytes());
        hasher.update(&[0x1D]);
        hasher.update(row.await_mode.as_deref().unwrap_or("").as_bytes());
        hasher.update(&[0x1D]);
        hasher.update(row.cancel_policy.as_deref().unwrap_or("").as_bytes());
        hasher.update(&[0x1D]);
        hasher.update(row.parent_tool_call_id.as_deref().unwrap_or("").as_bytes());
    }

    hasher.finalize().to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_signature_is_deterministic_under_row_reordering() {
        let row_a = PreviewSignatureRow {
            request_id: "req-a".into(),
            lifecycle_state: Some("processing".into()),
            await_mode: Some("foreground".into()),
            cancel_policy: Some("cascade".into()),
            parent_tool_call_id: Some("tc-1".into()),
        };
        let row_b = PreviewSignatureRow {
            request_id: "req-b".into(),
            lifecycle_state: Some("claimed".into()),
            await_mode: Some("background".into()),
            cancel_policy: Some("detach".into()),
            parent_tool_call_id: None,
        };
        let input_one = PreviewSignatureInput {
            root_request_id: "req-root".into(),
            root_state: Some("processing".into()),
            root_interrupt_requested_at: None,
            affected: vec![row_a.clone(), row_b.clone()],
        };
        let input_two = PreviewSignatureInput {
            root_request_id: "req-root".into(),
            root_state: Some("processing".into()),
            root_interrupt_requested_at: None,
            affected: vec![row_b, row_a],
        };

        assert_eq!(
            compute_preview_signature(&input_one),
            compute_preview_signature(&input_two)
        );
    }

    #[test]
    fn preview_signature_changes_when_root_state_changes() {
        let mut input = PreviewSignatureInput {
            root_request_id: "req-root".into(),
            root_state: Some("processing".into()),
            root_interrupt_requested_at: None,
            affected: vec![],
        };
        let before = compute_preview_signature(&input);
        input.root_state = Some("interrupted".into());
        let after = compute_preview_signature(&input);
        assert_ne!(before, after);
    }

    #[test]
    fn preview_signature_returns_lowercase_hex_64_chars() {
        let sig = compute_preview_signature(&PreviewSignatureInput {
            root_request_id: "req-root".into(),
            ..Default::default()
        });
        assert_eq!(sig.len(), 64, "BLAKE3 hex is 64 chars");
        assert!(sig
            .chars()
            .all(|c| c.is_ascii_hexdigit() && (c.is_ascii_digit() || c.is_ascii_lowercase())));
    }
}
