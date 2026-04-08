use super::*;

pub(super) async fn verify_ops_report_written(
    task_started_at: chrono::DateTime<Utc>,
    task_name: &str,
    ops_graphql_endpoint: &str,
) -> Result<String> {
    let since = task_started_at.to_rfc3339_opts(SecondsFormat::Secs, true);
    let query = format!(
        r#"{{ OpsReport(filter: {{ timestamp: {{ _gt: "{}" }} }}, order: {{ timestamp: DESC }}, limit: 1) {{ report_id status timestamp }} }}"#,
        since
    );
    let body = serde_json::json!({ "query": query });

    let client = reqwest::Client::new();
    let response = client
        .post(ops_graphql_endpoint)
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow!("failed to query observability-mcp for OpsReport: {e}"))?;

    let json: serde_json::Value = response
        .json()
        .await
        .map_err(|e| anyhow!("failed to parse OpsReport query response: {e}"))?;

    let reports = json
        .pointer("/data/OpsReport")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    if reports.is_empty() {
        anyhow::bail!(
            "task '{}' completed inference but did not write an OpsReport (agent must call write_ops_report before finishing)",
            task_name
        );
    }

    let status = reports[0]
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    tracing::info!(
        task = %task_name,
        reports_written = reports.len(),
        status = %status,
        "verified OpsReport(s) written for scheduled task"
    );

    Ok(status)
}

pub(super) async fn warn_if_missing_findings(
    task_started_at: chrono::DateTime<Utc>,
    task_name: &str,
    report_status: &str,
    ops_graphql_endpoint: &str,
) {
    let since = task_started_at.to_rfc3339_opts(SecondsFormat::Secs, true);
    let query = format!(
        r#"{{ OpsFinding(filter: {{ timestamp: {{ _gt: "{}" }} }}) {{ report_id }} }}"#,
        since
    );
    let body = serde_json::json!({ "query": query });

    let findings_count = match reqwest::Client::new()
        .post(ops_graphql_endpoint)
        .json(&body)
        .send()
        .await
    {
        Ok(resp) => match resp.json::<serde_json::Value>().await {
            Ok(json) => json
                .pointer("/data/OpsFinding")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0),
            Err(_) => return,
        },
        Err(_) => return,
    };

    if should_warn_missing_findings(report_status, findings_count) {
        tracing::warn!(
            task = %task_name,
            report_status = %report_status,
            "task wrote a {} report but no OpsFindings — model skipped structured findings",
            report_status
        );
    } else {
        tracing::info!(
            task = %task_name,
            findings_count = findings_count,
            "OpsFinding count for scheduled task"
        );
    }
}

pub(super) fn should_warn_missing_findings(report_status: &str, findings_count: usize) -> bool {
    findings_count == 0 && report_status != "healthy"
}

pub(super) async fn log_followup_consumption(
    task_started_at: chrono::DateTime<Utc>,
    task_name: &str,
    ops_graphql_endpoint: &str,
) {
    let since = task_started_at.to_rfc3339_opts(SecondsFormat::Secs, true);
    let query = format!(
        r#"{{ OpsFollowup(filter: {{ timestamp: {{ _lt: "{}" }} }}, order: {{ timestamp: DESC }}, limit: 20) {{ followup_id }} }}"#,
        since
    );
    let body = serde_json::json!({ "query": query });

    let followups_count = match reqwest::Client::new()
        .post(ops_graphql_endpoint)
        .json(&body)
        .send()
        .await
    {
        Ok(resp) => match resp.json::<serde_json::Value>().await {
            Ok(json) => json
                .pointer("/data/OpsFollowup")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0),
            Err(e) => {
                tracing::debug!(task = %task_name, error = %e, "followup context query: parse failed (advisory)");
                return;
            }
        },
        Err(e) => {
            tracing::debug!(task = %task_name, error = %e, "followup context query: request failed (advisory)");
            return;
        }
    };

    tracing::info!(
        task = %task_name,
        followups_available = followups_count,
        "OpsFollowup context for scheduled task"
    );
}
