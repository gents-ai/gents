//! BLAKE3 signature helpers and emit-floor state for the operations
//! snapshot watcher. See design spec lines 696-727 (previewSignature) and
//! 765-790 (emit floor).

use std::time::{Duration, Instant};

// --- Preview signature -------------------------------------------------

#[derive(Debug, Clone, Default)]
pub(crate) struct PreviewSignatureInput {
    pub root_request_id: String,
    pub root_state: Option<String>,
    pub root_interrupt_requested_at: Option<String>,
    pub affected: Vec<PreviewSignatureRow>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct PreviewSignatureRow {
    pub request_id: String,
    pub lifecycle_state: Option<String>,
    pub await_mode: Option<String>,
    pub cancel_policy: Option<String>,
    pub parent_tool_call_id: Option<String>,
}

pub(crate) fn compute_preview_signature(input: &PreviewSignatureInput) -> String {
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

// --- Liveness signature ------------------------------------------------
//
// Staged for the operations-rail liveness banner (operator-surfaces spec;
// landed with #310/#311) but not yet wired into the watcher — the emit-floor
// wiring is expected alongside the stream-liveness work (#437). Tested below;
// allow(dead_code) rather than deletion so the staged surface and its tests
// stay reviewable.

#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub(crate) struct LivenessSignatureInput {
    pub expired_processing_count: i64,
    pub active_native_executors_available: bool,
    pub requests: Vec<LivenessSignatureRequest>,
    pub tool_calls: Vec<LivenessSignatureToolCall>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub(crate) struct LivenessSignatureRequest {
    pub request_id: String,
    pub lifecycle_state: Option<String>,
    pub deadline_expired: bool,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub(crate) struct LivenessSignatureToolCall {
    pub tool_call_id: String,
    pub lifecycle_state: Option<String>,
    /// Included even though the design spec's listed field set (line 775)
    /// omits it: stuck-tool diagnostics depend on this transition, so a
    /// running tool crossing its deadline must invalidate the signature or
    /// the rail/banner would stay stale until some other change.
    pub deadline_expired: bool,
}

#[allow(dead_code)]
pub(crate) fn compute_liveness_signature(input: &LivenessSignatureInput) -> String {
    let mut hasher = blake3::Hasher::new();
    // Header: scalar fields.
    hasher.update(&input.expired_processing_count.to_le_bytes());
    hasher.update(&[0x1F]);
    hasher.update(&[input.active_native_executors_available as u8]);
    hasher.update(&[0x1E]);

    // Requests, sorted by request_id.
    let mut requests: Vec<&LivenessSignatureRequest> = input.requests.iter().collect();
    requests.sort_by(|a, b| a.request_id.cmp(&b.request_id));
    for (idx, row) in requests.iter().enumerate() {
        if idx > 0 {
            hasher.update(&[0x1F]);
        }
        hasher.update(row.request_id.as_bytes());
        hasher.update(&[0x1D]);
        hasher.update(row.lifecycle_state.as_deref().unwrap_or("").as_bytes());
        hasher.update(&[0x1D]);
        hasher.update(&[row.deadline_expired as u8]);
    }
    hasher.update(&[0x1E]);

    // Tool calls, sorted by tool_call_id.
    let mut tool_calls: Vec<&LivenessSignatureToolCall> = input.tool_calls.iter().collect();
    tool_calls.sort_by(|a, b| a.tool_call_id.cmp(&b.tool_call_id));
    for (idx, row) in tool_calls.iter().enumerate() {
        if idx > 0 {
            hasher.update(&[0x1F]);
        }
        hasher.update(row.tool_call_id.as_bytes());
        hasher.update(&[0x1D]);
        hasher.update(row.lifecycle_state.as_deref().unwrap_or("").as_bytes());
        hasher.update(&[0x1D]);
        hasher.update(&[row.deadline_expired as u8]);
    }

    hasher.finalize().to_hex().to_string()
}

// --- Emit floor --------------------------------------------------------

#[allow(dead_code)]
pub(crate) const EMIT_FLOOR_MIN_INTERVAL: Duration = Duration::from_millis(250);
#[allow(dead_code)]
pub(crate) const EMIT_FLOOR_MAX_COALESCE: Duration = Duration::from_secs(2);

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EmitDecision {
    /// Emit the new signature now.
    EmitNow,
    /// No structural change vs. the last observed/emitted signature; do not emit.
    NoChange,
    /// Structural change detected, but we must wait until `at` to emit (250ms floor).
    /// The watcher should arm a timer for `at` and re-call `observe` then.
    Defer { at: Instant },
}

#[allow(dead_code)]
#[derive(Debug, Default)]
pub(crate) struct LivenessEmitFloor {
    last_emitted_signature: Option<String>,
    last_emit_at: Option<Instant>,
    pending_change_first_seen_at: Option<Instant>,
}

#[allow(dead_code)]
impl LivenessEmitFloor {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Observe the latest signature at the wall-clock instant `now`.
    /// The watcher is expected to call this both on probe ticks and on
    /// any signal it has that the signature may have changed.
    pub(crate) fn observe(&mut self, signature: &str, now: Instant) -> EmitDecision {
        // Same signature as last emit: nothing to do; clear any pending change.
        if self.last_emitted_signature.as_deref() == Some(signature) {
            self.pending_change_first_seen_at = None;
            return EmitDecision::NoChange;
        }

        // Track when we first observed this changed signature so we can
        // honour the 2-second coalescing ceiling.
        let first_seen = *self.pending_change_first_seen_at.get_or_insert(now);

        // Inter-emit floor: 250ms minimum.
        if let Some(last) = self.last_emit_at {
            let since_last = now.saturating_duration_since(last);
            if since_last < EMIT_FLOOR_MIN_INTERVAL {
                // 2s coalescing ceiling — defensive backstop. Under normal use
                // the 250ms floor below fires first because we anchor to
                // last_emit, not last-observed.
                if now.saturating_duration_since(first_seen) >= EMIT_FLOOR_MAX_COALESCE {
                    self.commit_emit(signature, now);
                    return EmitDecision::EmitNow;
                }
                return EmitDecision::Defer {
                    at: last + EMIT_FLOOR_MIN_INTERVAL,
                };
            }
        }

        self.commit_emit(signature, now);
        EmitDecision::EmitNow
    }

    fn commit_emit(&mut self, signature: &str, now: Instant) {
        self.last_emitted_signature = Some(signature.to_owned());
        self.last_emit_at = Some(now);
        self.pending_change_first_seen_at = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

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

    #[test]
    fn liveness_signature_changes_when_expired_processing_count_changes() {
        let base = LivenessSignatureInput {
            expired_processing_count: 0,
            active_native_executors_available: true,
            requests: vec![LivenessSignatureRequest {
                request_id: "req-1".into(),
                lifecycle_state: Some("processing".into()),
                deadline_expired: false,
            }],
            tool_calls: vec![],
        };
        let with_expiry = LivenessSignatureInput {
            expired_processing_count: 1,
            ..base.clone()
        };
        assert_ne!(
            compute_liveness_signature(&base),
            compute_liveness_signature(&with_expiry)
        );
    }

    #[test]
    fn liveness_signature_is_stable_when_only_progress_age_drifts() {
        // The signature spec does NOT include lastProgressAgeMs — drift on age
        // alone must not invalidate the signature.
        let base = LivenessSignatureInput {
            expired_processing_count: 0,
            active_native_executors_available: true,
            requests: vec![LivenessSignatureRequest {
                request_id: "req-1".into(),
                lifecycle_state: Some("processing".into()),
                deadline_expired: false,
            }],
            tool_calls: vec![LivenessSignatureToolCall {
                tool_call_id: "tc-1".into(),
                lifecycle_state: Some("running".into()),
                deadline_expired: false,
            }],
        };
        assert_eq!(
            compute_liveness_signature(&base),
            compute_liveness_signature(&base.clone())
        );
    }

    #[test]
    fn liveness_signature_changes_when_tool_call_deadline_expires() {
        // Crossing a tool's deadline must change the signature even when
        // nothing else moves — stuck-tool diagnostics depend on this.
        let base = LivenessSignatureInput {
            expired_processing_count: 0,
            active_native_executors_available: true,
            requests: vec![LivenessSignatureRequest {
                request_id: "req-1".into(),
                lifecycle_state: Some("processing".into()),
                deadline_expired: false,
            }],
            tool_calls: vec![LivenessSignatureToolCall {
                tool_call_id: "tc-1".into(),
                lifecycle_state: Some("running".into()),
                deadline_expired: false,
            }],
        };
        let mut expired = base.clone();
        expired.tool_calls[0].deadline_expired = true;
        assert_ne!(
            compute_liveness_signature(&base),
            compute_liveness_signature(&expired)
        );
    }

    #[test]
    fn emit_floor_emits_on_first_observation() {
        let mut floor = LivenessEmitFloor::new();
        let now = t0();
        let decision = floor.observe("sig-a", now);
        assert_eq!(decision, EmitDecision::EmitNow);
    }

    #[test]
    fn emit_floor_returns_no_change_when_signature_unchanged() {
        let mut floor = LivenessEmitFloor::new();
        let now = t0();
        let _ = floor.observe("sig-a", now);
        let decision = floor.observe("sig-a", now + Duration::from_millis(500));
        assert_eq!(decision, EmitDecision::NoChange);
    }

    #[test]
    fn emit_floor_defers_within_250ms_window() {
        let mut floor = LivenessEmitFloor::new();
        let now = t0();
        let _ = floor.observe("sig-a", now);
        let decision = floor.observe("sig-b", now + Duration::from_millis(100));
        match decision {
            EmitDecision::Defer { at } => {
                assert_eq!(at, now + EMIT_FLOOR_MIN_INTERVAL);
            }
            other => panic!("expected Defer, got {other:?}"),
        }
    }

    #[test]
    fn emit_floor_emits_after_250ms_window() {
        let mut floor = LivenessEmitFloor::new();
        let now = t0();
        let _ = floor.observe("sig-a", now);
        let decision = floor.observe("sig-b", now + Duration::from_millis(260));
        assert_eq!(decision, EmitDecision::EmitNow);
    }

    #[test]
    fn emit_floor_emits_after_sustained_pending_period() {
        // After a Defer at t=50ms, if the watcher comes back at t=2100ms
        // (long past both the 250ms floor and the 2s ceiling) the call must
        // emit, never silently drop the pending change.
        let mut floor = LivenessEmitFloor::new();
        let now = t0();
        let _ = floor.observe("sig-a", now);

        let defer_decision = floor.observe("sig-b", now + Duration::from_millis(50));
        match defer_decision {
            EmitDecision::Defer { .. } => {}
            other => panic!("expected Defer at t=50ms, got {other:?}"),
        }

        let final_decision = floor.observe("sig-b", now + Duration::from_millis(2100));
        assert_eq!(final_decision, EmitDecision::EmitNow);
    }

    #[test]
    fn emit_floor_uses_latest_signature_in_trailing_emit() {
        let mut floor = LivenessEmitFloor::new();
        let now = t0();
        let _ = floor.observe("sig-a", now);
        let _ = floor.observe("sig-b", now + Duration::from_millis(50));
        let _ = floor.observe("sig-c", now + Duration::from_millis(200));
        let final_decision = floor.observe("sig-d", now + Duration::from_millis(260));
        assert_eq!(final_decision, EmitDecision::EmitNow);
        // After emit, the most recent signature ("sig-d") should be what's
        // recorded as last_emitted; observing it again should NoChange.
        assert_eq!(
            floor.observe("sig-d", now + Duration::from_millis(600)),
            EmitDecision::NoChange
        );
    }
}
