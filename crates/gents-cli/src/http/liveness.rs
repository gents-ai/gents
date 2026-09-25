use chrono::{DateTime, Utc};
use gents::tool_call_lifecycle::deadline_at_is_expired;
pub(crate) use gents_protocol::row::AgentRequestRow as LivenessRequestRow;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LivenessToolCallRow {
    pub(crate) request_id: String,
    #[serde(default)]
    pub(crate) agent_did: String,
    pub(crate) tool_call_id: String,
    pub(crate) tool_name: String,
    #[serde(default)]
    pub(crate) started_at: Option<String>,
    #[serde(default)]
    pub(crate) deadline_at: Option<String>,
    #[serde(default)]
    pub(crate) await_mode: Option<String>,
}

/// Durable activity for one processing request: a tool call (`started_at`,
/// `completed_at`) or an inference call (`started_at`, `ended_at`), including
/// finished ones. The newest of these timestamps and `claimed_at` is the
/// request's progress; rows are never deleted and timestamps are written once,
/// so that maximum cannot move back in time while the request advances.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct LivenessActivityRow {
    #[serde(default)]
    pub(crate) request_doc_id: Option<String>,
    /// Principal that wrote the row. Only rows written by the request's own
    /// agent count: another principal's row can name the same request
    /// document without being this request's progress.
    #[serde(default)]
    pub(crate) agent_did: Option<String>,
    #[serde(default)]
    pub(crate) started_at: Option<String>,
    #[serde(default)]
    pub(crate) completed_at: Option<String>,
    #[serde(default)]
    pub(crate) ended_at: Option<String>,
}

impl LivenessActivityRow {
    fn latest_at(&self) -> Option<DateTime<Utc>> {
        [&self.started_at, &self.completed_at, &self.ended_at]
            .into_iter()
            .filter_map(|value| parse_optional_rfc3339(value.as_deref()))
            .max()
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct RuntimeLivenessSnapshot {
    pub(crate) active_request_ids: Vec<String>,
    pub(crate) expired_processing_count: i64,
    pub(crate) ignored_foreign_processing_count: i64,
    pub(crate) requests: Vec<ActiveRequest>,
    pub(crate) active_tool_calls: Vec<ActiveToolCall>,
    pub(crate) ignored_foreign_tool_call_count: i64,
    pub(crate) active_native_executors_available: bool,
    pub(crate) active_native_executors: Vec<gents::NativeExecutorStatus>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ActiveRequest {
    pub(crate) request_id: String,
    pub(crate) claimed_at: Option<String>,
    pub(crate) deadline: Option<String>,
    pub(crate) deadline_expired: bool,
    pub(crate) deadline_age_ms: Option<i64>,
    pub(crate) last_progress_age_ms: i64,
    pub(crate) subagent_depth: i64,
    pub(crate) caused_by_parent_request_id: Option<String>,
    pub(crate) caused_by_trigger_kind: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ActiveToolCall {
    pub(crate) request_id: String,
    pub(crate) tool_call_id: String,
    pub(crate) tool_name: String,
    pub(crate) started_at: Option<String>,
    pub(crate) deadline_at: Option<String>,
    pub(crate) await_mode: Option<String>,
    pub(crate) running_age_ms: i64,
    pub(crate) deadline_expired: bool,
}

pub(crate) fn compute_request_liveness_summary(
    now: DateTime<Utc>,
    local_agent_did: &str,
    requests: Vec<LivenessRequestRow>,
    tool_calls: Vec<LivenessToolCallRow>,
    activity: Vec<LivenessActivityRow>,
) -> RuntimeLivenessSnapshot {
    let local_agent_did = local_agent_did.trim();
    let local_request_count = requests
        .iter()
        .filter(|row| {
            owns_liveness_row(
                local_agent_did,
                row.agent_did.as_deref().unwrap_or_default(),
            )
        })
        .count();
    let ignored_foreign_processing_count =
        requests.len().saturating_sub(local_request_count) as i64;
    let ignored_foreign_tool_call_count = tool_calls
        .iter()
        .filter(|row| !owns_liveness_row(local_agent_did, &row.agent_did))
        .count() as i64;

    let active_tool_calls: Vec<ActiveToolCall> = tool_calls
        .iter()
        .filter(|row| owns_liveness_row(local_agent_did, &row.agent_did))
        .map(|row| {
            let started_at = parse_optional_rfc3339(row.started_at.as_deref());
            let deadline_at = parse_optional_rfc3339(row.deadline_at.as_deref());
            let running_age_ms = started_at
                .map(|started| millis_between(started, now).max(0))
                .unwrap_or(0);
            let deadline_expired = deadline_at_is_expired(now, deadline_at);
            ActiveToolCall {
                request_id: row.request_id.clone(),
                tool_call_id: row.tool_call_id.clone(),
                tool_name: row.tool_name.clone(),
                started_at: row.started_at.clone(),
                deadline_at: row.deadline_at.clone(),
                await_mode: row.await_mode.clone(),
                running_age_ms,
                deadline_expired,
            }
        })
        .collect();

    let mut active_request_ids = Vec::with_capacity(local_request_count);
    let mut request_views = Vec::with_capacity(local_request_count);
    let mut expired_processing_count = 0i64;

    for row in requests.iter().filter(|row| {
        owns_liveness_row(
            local_agent_did,
            row.agent_did.as_deref().unwrap_or_default(),
        )
    }) {
        active_request_ids.push(row.request_id.clone());
        let claimed_at = parse_optional_rfc3339(row.claimed_at.as_deref());
        let deadline = parse_optional_rfc3339(row.deadline.as_deref());
        let deadline_expired = deadline_at_is_expired(now, deadline);
        if deadline_expired {
            expired_processing_count += 1;
        }
        let deadline_age_ms = deadline.map(|deadline| millis_between(deadline, now));

        let request_doc_id = row.doc_id.as_deref().map(str::trim);
        let request_agent_did = row.agent_did.as_deref().map(str::trim);
        let progress_at = activity
            .iter()
            .filter(|activity| {
                request_doc_id.is_some()
                    && request_agent_did.is_some()
                    && activity.request_doc_id.as_deref().map(str::trim) == request_doc_id
                    && activity.agent_did.as_deref().map(str::trim) == request_agent_did
            })
            .filter_map(LivenessActivityRow::latest_at)
            .chain(claimed_at)
            .max();
        let last_progress_age_ms = progress_at
            .map(|progress| millis_between(progress, now).max(0))
            .unwrap_or(0);

        request_views.push(ActiveRequest {
            request_id: row.request_id.clone(),
            claimed_at: row.claimed_at.clone(),
            deadline: row.deadline.clone(),
            deadline_expired,
            deadline_age_ms,
            last_progress_age_ms,
            subagent_depth: row.subagent_depth.unwrap_or(0),
            caused_by_parent_request_id: row.caused_by_parent_request_id.clone(),
            caused_by_trigger_kind: row.caused_by_trigger_kind.clone(),
        });
    }

    RuntimeLivenessSnapshot {
        active_request_ids,
        expired_processing_count,
        ignored_foreign_processing_count,
        requests: request_views,
        active_tool_calls,
        ignored_foreign_tool_call_count,
        active_native_executors_available: false,
        active_native_executors: Vec::new(),
    }
}

pub(crate) fn with_active_native_executors(
    mut snapshot: RuntimeLivenessSnapshot,
    active_native_executors: Vec<gents::NativeExecutorStatus>,
) -> RuntimeLivenessSnapshot {
    snapshot.active_native_executors_available = true;
    snapshot.active_native_executors = active_native_executors;
    snapshot
}

fn parse_optional_rfc3339(value: Option<&str>) -> Option<DateTime<Utc>> {
    let trimmed = value?.trim();
    if trimmed.is_empty() {
        return None;
    }
    DateTime::parse_from_rfc3339(trimmed)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

pub(crate) fn owns_liveness_row(local_agent_did: &str, row_agent_did: &str) -> bool {
    local_agent_did.is_empty() || row_agent_did.trim() == local_agent_did
}

fn millis_between(earlier: DateTime<Utc>, later: DateTime<Utc>) -> i64 {
    later.signed_duration_since(earlier).num_milliseconds()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::Utc.with_ymd_and_hms(2026, 5, 13, 12, 0, 0).unwrap()
    }

    fn iso(offset_secs: i64) -> String {
        (now() + chrono::Duration::seconds(offset_secs)).to_rfc3339()
    }

    fn request(
        request_id: &str,
        claimed_offset_secs: i64,
        deadline_offset_secs: i64,
    ) -> LivenessRequestRow {
        serde_json::from_value(serde_json::json!({
            "_docID": format!("doc-{request_id}"),
            "request_id": request_id,
            "agent_did": "did:test:local",
            "claimed_at": iso(claimed_offset_secs),
            "deadline": iso(deadline_offset_secs),
        }))
        .expect("canonical AgentRequest liveness row")
    }

    fn tool_call(
        request_id: &str,
        tool_call_id: &str,
        tool_name: &str,
        started_offset_secs: i64,
        deadline_offset_secs: Option<i64>,
        await_mode: Option<&str>,
    ) -> LivenessToolCallRow {
        LivenessToolCallRow {
            request_id: request_id.to_string(),
            agent_did: "did:test:local".to_string(),
            tool_call_id: tool_call_id.to_string(),
            tool_name: tool_name.to_string(),
            started_at: Some(iso(started_offset_secs)),
            deadline_at: deadline_offset_secs.map(iso),
            await_mode: await_mode.map(str::to_string),
        }
    }

    #[test]
    fn expired_processing_count_counts_requests_with_past_deadline() {
        let requests = vec![
            request("req-expired", -120, -30),
            request("req-fresh", -10, 60),
        ];
        let snapshot = compute_request_liveness_summary(
            now(),
            "did:test:local",
            requests,
            Vec::new(),
            Vec::new(),
        );

        assert_eq!(snapshot.expired_processing_count, 1);
        assert!(snapshot
            .active_request_ids
            .iter()
            .any(|id| id == "req-expired"));
        let expired = snapshot
            .requests
            .iter()
            .find(|r| r.request_id == "req-expired")
            .expect("expired request must appear in snapshot");
        assert!(
            expired.deadline_expired,
            "deadline should be flagged expired"
        );
        let fresh = snapshot
            .requests
            .iter()
            .find(|r| r.request_id == "req-fresh")
            .expect("fresh request must appear in snapshot");
        assert!(
            !fresh.deadline_expired,
            "fresh deadline must not be flagged"
        );
    }

    #[test]
    fn active_tool_calls_carry_tool_name_and_running_age() {
        let requests = vec![request("req-1", -45, 60)];
        let tools = vec![tool_call("req-1", "tc-1", "glob", -30, Some(60), None)];
        let snapshot =
            compute_request_liveness_summary(now(), "did:test:local", requests, tools, Vec::new());

        assert_eq!(snapshot.active_tool_calls.len(), 1);
        let tc = &snapshot.active_tool_calls[0];
        assert_eq!(tc.tool_name, "glob");
        assert_eq!(tc.request_id, "req-1");
        assert!(
            tc.running_age_ms >= 30_000,
            "running age must reflect 30s elapsed, got {}",
            tc.running_age_ms
        );
        assert!(!tc.deadline_expired);
    }

    #[test]
    fn subagent_bridge_tool_calls_carry_await_mode() {
        let requests = vec![request("req-parent", -10, 300)];
        let tools = vec![tool_call(
            "req-parent",
            "tc-bridge",
            "amy-rumination",
            -5,
            None,
            Some("bridge"),
        )];
        let snapshot =
            compute_request_liveness_summary(now(), "did:test:local", requests, tools, Vec::new());

        let tc = &snapshot.active_tool_calls[0];
        assert_eq!(tc.await_mode.as_deref(), Some("bridge"));
    }

    fn tool_activity(
        request_id: &str,
        started_offset_secs: i64,
        completed_offset_secs: Option<i64>,
    ) -> LivenessActivityRow {
        LivenessActivityRow {
            request_doc_id: Some(format!("doc-{request_id}")),
            agent_did: Some("did:test:local".to_string()),
            started_at: Some(iso(started_offset_secs)),
            completed_at: completed_offset_secs.map(iso),
            ended_at: None,
        }
    }

    fn inference_activity(
        request_id: &str,
        started_offset_secs: i64,
        ended_offset_secs: Option<i64>,
    ) -> LivenessActivityRow {
        LivenessActivityRow {
            request_doc_id: Some(format!("doc-{request_id}")),
            agent_did: Some("did:test:local".to_string()),
            started_at: Some(iso(started_offset_secs)),
            completed_at: None,
            ended_at: ended_offset_secs.map(iso),
        }
    }

    fn progress_age_ms(
        at: chrono::DateTime<chrono::Utc>,
        tools: Vec<LivenessToolCallRow>,
        activity: Vec<LivenessActivityRow>,
    ) -> i64 {
        let snapshot = compute_request_liveness_summary(
            at,
            "did:test:local",
            vec![request("req-1", -300, 600)],
            tools,
            activity,
        );
        snapshot.requests[0].last_progress_age_ms
    }

    #[test]
    fn last_progress_age_ms_uses_running_tool_start_over_claimed_at() {
        let age = progress_age_ms(
            now(),
            vec![tool_call("req-1", "tc-1", "bash", -10, Some(60), None)],
            vec![tool_activity("req-1", -10, None)],
        );
        assert_eq!(age, 10_000, "running tool started 10s ago beats claim");
    }

    #[test]
    fn last_progress_age_ms_uses_newest_completed_tool_call_with_none_active() {
        let activity = (0..200)
            .map(|i| tool_activity("req-1", -290 + i, Some(-289 + i)))
            .chain([tool_activity("req-1", -8, Some(-4))])
            .collect();
        let age = progress_age_ms(now(), Vec::new(), activity);
        assert_eq!(
            age, 4_000,
            "newest completion (4s ago) is progress, not claimed_at (300s ago)"
        );
    }

    #[test]
    fn in_flight_inference_between_tool_batches_is_not_stale() {
        let activity = vec![
            tool_activity("req-1", -120, Some(-90)),
            inference_activity("req-1", -80, Some(-60)),
            inference_activity("req-1", -3, None),
        ];
        let age = progress_age_ms(now(), Vec::new(), activity);
        assert_eq!(age, 3_000, "in-flight inference started 3s ago");
    }

    #[test]
    fn foreign_request_activity_does_not_count_as_progress() {
        let mut other = tool_activity("req-other", -1, Some(0));
        other.request_doc_id = Some("doc-req-other".to_string());
        let age = progress_age_ms(now(), Vec::new(), vec![other]);
        assert_eq!(age, 300_000, "only this request's activity counts");
    }

    #[test]
    fn foreign_principal_row_on_same_request_doc_does_not_count_as_progress() {
        let mut foreign_tool = tool_activity("req-1", -2, Some(-1));
        foreign_tool.agent_did = Some("did:test:foreign".to_string());
        let mut foreign_inference = inference_activity("req-1", -1, None);
        foreign_inference.agent_did = Some("did:test:foreign".to_string());
        let own = tool_activity("req-1", -100, Some(-90));
        let age = progress_age_ms(
            now(),
            Vec::new(),
            vec![own, foreign_tool, foreign_inference],
        );
        assert_eq!(
            age, 90_000,
            "a newer row by another principal naming the same request doc is ignored"
        );
    }

    /// #1782: a tool call leaving the running set must not move progress
    /// back to `claimed_at`. Across samples of an advancing request, progress
    /// time never decreases and the age never exceeds the gap since the last
    /// durable event.
    #[test]
    fn progress_never_moves_back_in_time_across_samples() {
        let running_read = || vec![tool_call("req-1", "tc-1", "read_file", -1, None, None)];
        // (sample offset secs, running tool calls, durable activity)
        let samples = vec![
            (0, running_read(), vec![tool_activity("req-1", -1, None)]),
            // tc-1 completed: it left the running set.
            (5, Vec::new(), vec![tool_activity("req-1", -1, Some(2))]),
            // Waiting on the model between tool batches.
            (
                10,
                Vec::new(),
                vec![
                    tool_activity("req-1", -1, Some(2)),
                    inference_activity("req-1", 6, None),
                ],
            ),
            (
                20,
                Vec::new(),
                vec![
                    tool_activity("req-1", -1, Some(2)),
                    inference_activity("req-1", 6, Some(18)),
                ],
            ),
            (
                25,
                Vec::new(),
                vec![
                    tool_activity("req-1", -1, Some(2)),
                    inference_activity("req-1", 6, Some(18)),
                    tool_activity("req-1", 21, Some(24)),
                ],
            ),
        ];
        let mut previous_progress_at = None;
        for (offset, running, activity) in samples {
            let sample_at = now() + chrono::Duration::seconds(offset);
            let age = progress_age_ms(sample_at, running, activity);
            assert!(
                age <= 5_000,
                "sample +{offset}s: advancing request aged {age}ms"
            );
            let progress_at = sample_at - chrono::Duration::milliseconds(age);
            if let Some(previous) = previous_progress_at {
                assert!(
                    progress_at >= previous,
                    "sample +{offset}s: progress moved back from {previous} to {progress_at}"
                );
            }
            previous_progress_at = Some(progress_at);
        }
    }

    #[test]
    fn last_progress_age_ms_falls_back_to_claimed_at_when_no_tool_calls() {
        let requests = vec![request("req-1", -45, 60)];
        let snapshot = compute_request_liveness_summary(
            now(),
            "did:test:local",
            requests,
            Vec::new(),
            Vec::new(),
        );

        let req = &snapshot.requests[0];
        assert!(
            req.last_progress_age_ms >= 45_000,
            "claimed 45s ago, got {}",
            req.last_progress_age_ms
        );
    }

    #[test]
    fn foreign_processing_requests_do_not_count_as_active_or_expired() {
        let mut foreign = request("req-foreign", -120, -30);
        foreign.agent_did = Some("did:test:foreign".to_string());
        let requests = vec![request("req-local", -120, -30), foreign];

        let snapshot = compute_request_liveness_summary(
            now(),
            "did:test:local",
            requests,
            Vec::new(),
            Vec::new(),
        );

        assert_eq!(snapshot.expired_processing_count, 1);
        assert_eq!(snapshot.ignored_foreign_processing_count, 1);
        assert_eq!(snapshot.active_request_ids, vec!["req-local".to_string()]);
        assert_eq!(snapshot.requests.len(), 1);
        assert_eq!(snapshot.requests[0].request_id, "req-local");
    }

    #[test]
    fn foreign_running_tool_calls_are_ignored() {
        let requests = vec![request("req-local", -45, 60)];
        let mut foreign_tool = tool_call("req-foreign", "tc-foreign", "bash", -30, None, None);
        foreign_tool.agent_did = "did:test:foreign".to_string();
        let tools = vec![
            tool_call("req-local", "tc-local", "glob", -30, None, None),
            foreign_tool,
        ];

        let snapshot =
            compute_request_liveness_summary(now(), "did:test:local", requests, tools, Vec::new());

        assert_eq!(snapshot.active_tool_calls.len(), 1);
        assert_eq!(snapshot.active_tool_calls[0].tool_call_id, "tc-local");
        assert_eq!(snapshot.ignored_foreign_tool_call_count, 1);
    }
}
