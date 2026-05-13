use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LivenessRequestRow {
    pub(crate) request_id: String,
    #[serde(default)]
    pub(crate) claimed_at: Option<String>,
    #[serde(default)]
    pub(crate) deadline: Option<String>,
    #[serde(default)]
    pub(crate) subagent_depth: Option<i64>,
    #[serde(default)]
    pub(crate) caused_by_parent_request_id: Option<String>,
    #[serde(default)]
    pub(crate) caused_by_trigger_kind: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LivenessToolCallRow {
    pub(crate) request_id: String,
    pub(crate) tool_call_id: String,
    pub(crate) tool_name: String,
    #[serde(default)]
    pub(crate) started_at: Option<String>,
    #[serde(default)]
    pub(crate) deadline_at: Option<String>,
    #[serde(default)]
    pub(crate) await_mode: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct RuntimeLivenessSnapshot {
    pub(crate) active_request_ids: Vec<String>,
    pub(crate) expired_processing_count: i64,
    pub(crate) requests: Vec<ActiveRequest>,
    pub(crate) active_tool_calls: Vec<ActiveToolCall>,
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

pub(crate) fn compute_liveness_summary(
    now: DateTime<Utc>,
    requests: Vec<LivenessRequestRow>,
    tool_calls: Vec<LivenessToolCallRow>,
) -> RuntimeLivenessSnapshot {
    let active_tool_calls: Vec<ActiveToolCall> = tool_calls
        .iter()
        .map(|row| {
            let started_at = parse_optional_rfc3339(row.started_at.as_deref());
            let deadline_at = parse_optional_rfc3339(row.deadline_at.as_deref());
            let running_age_ms = started_at
                .map(|started| millis_between(started, now).max(0))
                .unwrap_or(0);
            let deadline_expired = deadline_at.is_some_and(|deadline| now > deadline);
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

    let mut active_request_ids = Vec::with_capacity(requests.len());
    let mut request_views = Vec::with_capacity(requests.len());
    let mut expired_processing_count = 0i64;

    for row in &requests {
        active_request_ids.push(row.request_id.clone());
        let claimed_at = parse_optional_rfc3339(row.claimed_at.as_deref());
        let deadline = parse_optional_rfc3339(row.deadline.as_deref());
        let deadline_expired = deadline.is_some_and(|deadline| now > deadline);
        if deadline_expired {
            expired_processing_count += 1;
        }
        let deadline_age_ms = deadline.map(|deadline| millis_between(deadline, now));

        let latest_tool_activity = active_tool_calls
            .iter()
            .filter(|tc| tc.request_id == row.request_id)
            .filter_map(|tc| parse_optional_rfc3339(tc.started_at.as_deref()))
            .max();
        let progress_at = match (claimed_at, latest_tool_activity) {
            (Some(claimed), Some(tool)) => Some(claimed.max(tool)),
            (Some(claimed), None) => Some(claimed),
            (None, Some(tool)) => Some(tool),
            (None, None) => None,
        };
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
        requests: request_views,
        active_tool_calls,
    }
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
        LivenessRequestRow {
            request_id: request_id.to_string(),
            claimed_at: Some(iso(claimed_offset_secs)),
            deadline: Some(iso(deadline_offset_secs)),
            subagent_depth: None,
            caused_by_parent_request_id: None,
            caused_by_trigger_kind: None,
        }
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
            request("req-expired", -120, -30), // claimed 2m ago, deadline 30s ago
            request("req-fresh", -10, 60),     // claimed 10s ago, deadline 60s in future
        ];
        let snapshot = compute_liveness_summary(now(), requests, Vec::new());

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
        let snapshot = compute_liveness_summary(now(), requests, tools);

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
        let snapshot = compute_liveness_summary(now(), requests, tools);

        let tc = &snapshot.active_tool_calls[0];
        assert_eq!(tc.await_mode.as_deref(), Some("bridge"));
    }

    #[test]
    fn last_progress_age_ms_uses_most_recent_tool_activity_over_claimed_at() {
        let requests = vec![request("req-1", -300, 60)];
        let tools = vec![tool_call("req-1", "tc-1", "bash", -10, Some(60), None)];
        let snapshot = compute_liveness_summary(now(), requests, tools);

        let req = snapshot
            .requests
            .iter()
            .find(|r| r.request_id == "req-1")
            .unwrap();
        assert!(
            req.last_progress_age_ms < 60_000,
            "tool started 10s ago must beat claimed_at 300s ago, got {}",
            req.last_progress_age_ms
        );
        assert!(
            req.last_progress_age_ms >= 10_000,
            "progress age must reflect tool start, got {}",
            req.last_progress_age_ms
        );
    }

    #[test]
    fn last_progress_age_ms_falls_back_to_claimed_at_when_no_tool_calls() {
        let requests = vec![request("req-1", -45, 60)];
        let snapshot = compute_liveness_summary(now(), requests, Vec::new());

        let req = &snapshot.requests[0];
        assert!(
            req.last_progress_age_ms >= 45_000,
            "claimed 45s ago, got {}",
            req.last_progress_age_ms
        );
    }
}
