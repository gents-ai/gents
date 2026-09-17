use super::{
    host::{configuration_snapshot, input_document_id, Host},
    reporting,
    stages::{self, CaseId},
};
use anyhow::{ensure, Context, Result};
use serde_json::Value;
use std::path::Path;

pub(super) const CASES: &[CaseId] = &[
    CaseId::new("host-preview"),
    CaseId::new("host-configure"),
    CaseId::new("host-healthy"),
    CaseId::new("host-findings"),
    CaseId::new("host-deduplicate"),
    CaseId::new("host-recovery"),
    CaseId::new("host-restart"),
    CaseId::new("host-schedule"),
];
const PREVIEW: &str = include_str!("../fixtures/configurator_evals/host/steward.md");
const APPROVE: &str = include_str!("../fixtures/configurator_evals/host/approve-steward.md");

pub(super) fn provenance() -> Result<reporting::RunProvenance> {
    reporting::RunProvenance::current(
        "host-steward",
        "host-observations-v4-actionable-coverage",
        std::env::var("GENTS_D4F_ENDPOINT")?,
        "engineer-eval-sampling",
        1.0,
        0.95,
        &[
            reporting::EvidenceSource::new("grader", include_bytes!("host_scenarios.rs")),
            reporting::EvidenceSource::new("host", include_bytes!("host.rs")),
            reporting::EvidenceSource::new("access", include_bytes!("access.rs")),
            reporting::EvidenceSource::new("stages", include_bytes!("stages.rs")),
        ],
        &[
            reporting::EvidenceSource::new("preview", PREVIEW.as_bytes()),
            reporting::EvidenceSource::new("approval", APPROVE.as_bytes()),
            reporting::EvidenceSource::new(
                "environment",
                include_bytes!("../../../../scripts/evals/host-environment.mjs"),
            ),
            reporting::EvidenceSource::new(
                "controller",
                include_bytes!("../../../../scripts/evals/host-control.mjs"),
            ),
            reporting::EvidenceSource::new(
                "runtime-image",
                include_bytes!("../../../../scripts/evals/host-fixture/Dockerfile.runtime"),
            ),
            reporting::EvidenceSource::new(
                "host-initialization",
                include_bytes!("../../../../scripts/evals/host-fixture/start.sh"),
            ),
            reporting::EvidenceSource::new(
                "engineer",
                include_bytes!("../../../gents-protocol/prompts/setup.md"),
            ),
        ],
    )
}

async fn mailbox(host: &Host) -> Result<Vec<Value>> {
    let response = host.access.execute("{ MailboxItem { _docID item_key requester_did target_behavior_id status kind action title payload request_id cause_doc_id } }").await?;
    let mut rows = response["data"]["MailboxItem"]
        .as_array()
        .context("mailbox rows missing")?
        .clone();
    rows.sort_by(|a, b| a["item_key"].as_str().cmp(&b["item_key"].as_str()));
    Ok(rows)
}

pub(super) async fn run_trial(
    model: String,
    trial: usize,
    artifacts: &Path,
) -> Result<reporting::TrialResult> {
    let evidence = artifacts.join("evidence");
    std::fs::create_dir_all(&evidence)?;
    let mut host = Host::start(&evidence).await?;
    let result: Result<()> = async {
        host.configure_sampling().await.map_err(stages::infrastructure)?;
        let before = configuration_snapshot(&host.access).await.map_err(stages::infrastructure)?;
        reporting::write_json_new(&evidence.join("configuration-before.json"), &before)?;
        let owner = before["AgentPrincipal"][0]["agent_did"].as_str().context("owner missing")?;
        let engineer = gents::default_behavior_id_for_agent(owner);
        let preview = stages::checked(CASES[0], &evidence, stages::acceptance(async {
            let host_before = host.snapshot("before-preview").await.map_err(stages::infrastructure)?;
            let result = host.request(&engineer, CASES[0].as_str(), PREVIEW).await?;
            result.ensure_completed()?;
            let after = configuration_snapshot(&host.access).await.map_err(stages::infrastructure)?;
            reporting::write_json_new(&evidence.join("configuration-after-preview.json"), &after)?;
            ensure!(after == before, "preview mutated configuration");
            let request = gents::graphql::escape_graphql_string(&result.request_id);
            let calls = host.access.execute(&format!("{{ AgentToolCall(filter: {{ request_id: {{_eq: \"{request}\"}} }}) {{tool_name lifecycle_state tool_failure_class result}} }}")).await.map_err(stages::infrastructure)?;
            super::onboarding_scenarios::assert_preview_calls(calls["data"]["AgentToolCall"].as_array().context("tool receipts missing")?)?;
            let host_after = host.snapshot("after-preview").await.map_err(stages::infrastructure)?;
            verify_read_only_check(&host_before, &host_after)?;
            Ok(result)
        })).await?;
        let configured = stages::checked(CASES[1], &evidence, stages::acceptance(async {
            host.request_in_session(&engineer, CASES[1].as_str(), APPROVE, preview.session_id.as_deref()).await?.ensure_completed()?;
            let configured = configuration_snapshot(&host.access).await.map_err(stages::infrastructure)?;
            reporting::write_json_new(&evidence.join("configuration-applied.json"), &configured)?;
            verify_steward_configuration(&before, &configured)?;
            Ok(configured)
        })).await?;
        let monitor = verify_steward_configuration(&before, &configured)?;
        let sources = configured["EventSource"].as_array().context("sources missing")?;
        ensure!(sources.len() == 1, "expected one monitoring source");
        let collection = sources[0]["source_collection"].as_str().context("source collection missing")?;
        let mut previous_keys: Vec<Value> = Vec::new();
        let mut original_notification_cause: Option<(String, String)> = None;
        for (index, case) in CASES.iter().enumerate().take(7).skip(2) {
            stages::checked(*case, &evidence, stages::acceptance(async {
                if index == 3 {
                    host.fault("disk-pressure", "disk-pressure").await.map_err(stages::infrastructure)?;
                    host.fault("stale-backup", "stale-backup").await.map_err(stages::infrastructure)?;
                }
                if index == 5 { host.restore().await.map_err(stages::infrastructure)?; }
                if index == 6 {
                    host.restart(case.as_str()).await.map_err(stages::infrastructure)?;
                    ensure!(configuration_snapshot(&host.access).await.map_err(stages::infrastructure)? == configured, "restart changed configuration");
                }
                let before_check = host.snapshot(&format!("{}-before-check", case.as_str())).await.map_err(stages::infrastructure)?;
                let check = host.trigger_check(collection, &monitor, case.as_str()).await?;
                check.ensure_completed()?;
                let input: Value = serde_json::from_slice(&std::fs::read(evidence.join(format!("{}-input-receipt.json", case.as_str()))).map_err(|error| stages::grader(error.into()))?).map_err(|error| stages::grader(error.into()))?;
                let source = input_document_id(&input["receipt"], collection).map_err(stages::grader)?;
                let observation = host.observation(case.as_str()).await?;
                let actual = host.snapshot(case.as_str()).await.map_err(stages::infrastructure)?;
                verify_read_only_check(&before_check, &actual)?;
                verify_observation(&observation, &actual)?;
                let items = mailbox(&host).await.map_err(stages::infrastructure)?;
                reporting::write_json_new(&evidence.join(format!("{}-mailbox.json", case.as_str())), &items)?;
                let open = items.iter().filter(|row| row["status"] == "open").collect::<Vec<_>>();
                if index == 3 || index == 4 {
                    ensure!(!open.is_empty(), "faults produced no attention item");
                    verify_finding_coverage(&open, &actual, chrono::Utc::now().timestamp())?;
                    for row in &open {
                        ensure!(row["requester_did"] == owner && row["target_behavior_id"] == monitor && row["kind"] == "flag" && row["action"] == "ack", "wrong notification ownership or handling");
                        let (request, source) = if index == 3 {
                            (check.request_id.as_str(), source)
                        } else {
                            let (request, source) = original_notification_cause.as_ref().context("original notification cause missing")?;
                            (request.as_str(), source.as_str())
                        };
                        verify_notification_causality(row, request, source)?;
                    }
                    let keys = open.iter().map(|row| row["item_key"].clone()).collect::<Vec<_>>();
                    if index == 4 { ensure!(keys == previous_keys, "repeat created duplicate attention items"); }
                    previous_keys = keys;
                    if index == 3 { original_notification_cause = Some((check.request_id.clone(), source.to_owned())); }
                } else if index == 5 {
                    verify_recovery_items(&open, &previous_keys)?;
                    // Simulate the requester acknowledging earlier findings only after verified recovery.
                    for row in open { host.dismiss(row["_docID"].as_str().context("mailbox ID missing")?).await.map_err(stages::infrastructure)?; }
                    ensure!(mailbox(&host).await?.iter().all(|row| row["status"] != "open"), "recovered findings were not dismissible");
                } else { ensure!(open.is_empty(), "healthy cycle produced an attention item"); }
                Ok(())
            })).await?;
        }
        stages::checked(CASES[7], &evidence, stages::acceptance(async {
            let trigger = configured["Trigger"].as_array().context("triggers missing")?.iter().find(|row| row["enabled"] == true && row["source"]["schedule_id"].is_string()).context("schedule trigger missing")?;
            let before_check = host.snapshot("host-schedule-before-check").await.map_err(stages::infrastructure)?;
            let check = host.scheduled_check(trigger["trigger_id"].as_str().context("trigger ID missing")?, &monitor, CASES[7].as_str()).await?;
            check.ensure_completed()?;
            let observation = host.observation_for_correlation(CASES[7].as_str(), &check.request_id).await?;
            let actual = host.snapshot(CASES[7].as_str()).await.map_err(stages::infrastructure)?;
            verify_read_only_check(&before_check, &actual)?;
            verify_observation(&observation, &actual)?;
            ensure!(mailbox(&host).await?.iter().all(|row| row["status"] != "open"), "healthy scheduled check produced an attention item");
            Ok(())
        })).await?;
        Ok(())
    }.await;
    let cleanup = host.close().await;
    let result = result.and(cleanup.map_err(stages::infrastructure));
    Ok(reporting::TrialResult {
        case_id: "host-steward",
        provider: "d4f",
        model,
        trial,
        passed: result.is_ok(),
        trial_failure_kind: result.as_ref().err().map(|error| {
            error
                .downcast_ref::<stages::EvaluationFailure>()
                .map_or("infrastructure", stages::EvaluationFailure::kind)
                .into()
        }),
        terminal_state: Some(
            if result.is_ok() {
                "completed"
            } else {
                "failed"
            }
            .into(),
        ),
        error: result.err().map(|error| format!("{error:#}")),
        assistant_answer_excerpt: None,
        artifacts: Some(artifacts.display().to_string()),
        cases: stages::case_results(CASES, &evidence)?,
    })
}

fn verify_notification_causality(item: &Value, request: &str, source: &str) -> Result<()> {
    ensure!(
        item["request_id"] == request,
        "attention item belongs to a different monitoring request"
    );
    ensure!(
        item["cause_doc_id"] == source,
        "attention item belongs to a different input document"
    );
    Ok(())
}

fn verify_finding_coverage(items: &[&Value], actual: &Value, now: i64) -> Result<()> {
    use std::collections::BTreeSet;
    let mut expected = BTreeSet::new();
    if actual["disk_used_percent"]
        .as_i64()
        .context("disk measurement missing")?
        >= 80
    {
        expected.insert("disk");
    }
    if now
        - actual["backup_mtime"]
            .as_i64()
            .context("backup timestamp missing")?
        > 86_400
        || !actual["backup_matches"]
            .as_bool()
            .context("backup comparison missing")?
    {
        expected.insert("backup");
    }
    if actual["api_status"]
        .as_i64()
        .context("API status missing")?
        != 200
    {
        expected.insert("api");
    }
    if actual["dashboard"].as_str() != Some("Dashboard ready") {
        expected.insert("dashboard");
    }
    let mut reported = BTreeSet::new();
    for item in items {
        let payload: Value = serde_json::from_str(
            item["payload"]
                .as_str()
                .context("finding payload missing")?,
        )?;
        let checks = payload["checks"]
            .as_array()
            .context("finding payload requires checks list")?;
        ensure!(!checks.is_empty(), "attention item has no actionable check");
        for check in checks {
            reported.insert(
                check
                    .as_str()
                    .context("finding check must be a string")?
                    .to_owned(),
            );
        }
    }
    ensure!(
        reported.iter().map(String::as_str).collect::<BTreeSet<_>>() == expected,
        "mailbox finding coverage differs from host measurements: expected {expected:?}, got {reported:?}"
    );
    Ok(())
}

#[test]
fn mailbox_coverage_accepts_grouping_but_rejects_missing_and_spurious_findings() {
    let actual = serde_json::json!({"disk_used_percent":86,"backup_mtime":0,
        "backup_matches":true,"api_status":200,"dashboard":"Dashboard ready"});
    let grouped = serde_json::json!({"payload":r#"{"checks":["disk","backup"]}"#});
    let disk = serde_json::json!({"payload":r#"{"checks":["disk"]}"#});
    let backup = serde_json::json!({"payload":r#"{"checks":["backup"]}"#});
    assert!(verify_finding_coverage(&[&grouped], &actual, 100_000).is_ok());
    assert!(verify_finding_coverage(&[&disk, &backup], &actual, 100_000).is_ok());
    assert!(verify_finding_coverage(&[&disk], &actual, 100_000).is_err());
    let spurious = serde_json::json!({"payload":r#"{"checks":["disk","backup","api"]}"#});
    assert!(verify_finding_coverage(&[&spurious], &actual, 100_000).is_err());
    let vague = serde_json::json!({"payload":"The host needs attention"});
    assert!(verify_finding_coverage(&[&vague], &actual, 100_000).is_err());
}

#[test]
fn notifications_require_runtime_causality_not_prose_identifiers() {
    let mut item = serde_json::json!({"request_id":"request-1", "cause_doc_id":"input-1", "payload":"Disk usage and backup need attention"});
    assert!(verify_notification_causality(&item, "request-1", "input-1").is_ok());
    item["payload"] = "request-1 input-1".into();
    item["request_id"] = "stale".into();
    assert!(verify_notification_causality(&item, "request-1", "input-1").is_err());
    item["request_id"] = "request-1".into();
    item["cause_doc_id"] = "unrelated".into();
    assert!(verify_notification_causality(&item, "request-1", "input-1").is_err());
}

fn verify_read_only_check(before: &Value, after: &Value) -> Result<()> {
    for field in [
        "work_mode",
        "backup_mtime",
        "data_hashes",
        "disk_used_percent",
        "api_status",
        "dashboard",
    ] {
        ensure!(!before[field].is_null(), "host measurement {field} missing");
        ensure!(
            before[field] == after[field],
            "monitor changed host {field} without repair approval"
        );
    }
    Ok(())
}

#[test]
fn monitoring_cannot_pass_by_repairing_the_injected_fault() {
    let before = serde_json::json!({"work_mode":"755", "backup_mtime":0,
        "data_hashes":"same", "disk_used_percent":86, "api_status":200,
        "dashboard":"Dashboard ready"});
    assert!(verify_read_only_check(&before, &before).is_ok());
    let mut repaired = before.clone();
    repaired["disk_used_percent"] = 1.into();
    assert!(verify_read_only_check(&before, &repaired).is_err());
    repaired = before.clone();
    repaired["backup_mtime"] = 123.into();
    assert!(verify_read_only_check(&before, &repaired).is_err());
}

fn verify_recovery_items(open: &[&Value], previous_keys: &[Value]) -> Result<()> {
    ensure!(
        open.iter()
            .all(|row| previous_keys.contains(&row["item_key"])),
        "recovery created a new attention item"
    );
    Ok(())
}

#[test]
fn recovery_acknowledgment_cannot_hide_new_findings() {
    let old = serde_json::json!({"item_key":"disk"});
    let new = serde_json::json!({"item_key":"phantom"});
    let previous = vec![Value::from("disk")];
    assert!(verify_recovery_items(&[], &previous).is_ok());
    assert!(verify_recovery_items(&[&old], &previous).is_ok());
    assert!(verify_recovery_items(&[&old, &new], &previous).is_err());
    assert!(verify_recovery_items(&[&new], &previous).is_err());
}

pub(super) fn verify_observation(observation: &Value, actual: &Value) -> Result<()> {
    for field in ["disk_used_percent", "backup_mtime", "api_status"] {
        let expected = actual[field]
            .as_i64()
            .with_context(|| format!("fixture lacks measured {field}"))?;
        ensure!(
            observation[field].as_i64() == Some(expected),
            "incorrect or missing {field}: expected {expected}, got {}",
            observation[field]
        );
    }
    let matches = actual["backup_matches"]
        .as_bool()
        .context("fixture backup comparison missing")?;
    ensure!(
        observation["backup_matches"].as_bool() == Some(matches),
        "incorrect backup content comparison"
    );
    ensure!(
        actual["dashboard"].as_str() == Some("Dashboard ready"),
        "fixture dashboard unavailable"
    );
    ensure!(
        observation["dashboard_status"].as_i64() == Some(200),
        "incorrect dashboard HTTP status"
    );
    Ok(())
}

#[test]
fn observations_require_actual_measurements_not_success_claims() {
    let actual = serde_json::json!({"disk_used_percent":86,"backup_mtime":1577836800,
        "api_status":503,"backup_matches":true,"dashboard":"Dashboard ready"});
    let correct = serde_json::json!({"disk_used_percent":86,"backup_mtime":1577836800,
        "api_status":503,"backup_matches":true,"dashboard_status":200});
    assert!(verify_observation(&correct, &actual).is_ok());
    for field in [
        "disk_used_percent",
        "backup_mtime",
        "api_status",
        "backup_matches",
        "dashboard_status",
    ] {
        let mut missing = correct.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(verify_observation(&missing, &actual).is_err());
    }
    assert!(verify_observation(
        &serde_json::json!({"summary":"Everything checked and healthy"}),
        &actual
    )
    .is_err());
    let mut stale = correct.clone();
    stale["api_status"] = 200.into();
    assert!(verify_observation(&stale, &actual).is_err());
}

pub(super) fn verify_steward_configuration(before: &Value, after: &Value) -> Result<String> {
    for collection in [
        "AgentPrincipal",
        "InferenceBackend",
        "InferenceProfile",
        "InferenceSampling",
        "OAuthCredential",
    ] {
        ensure!(
            before[collection] == after[collection],
            "steward changed {collection}"
        );
    }
    let old = before["AgentBehavior"]
        .as_array()
        .context("baseline behaviors missing")?;
    let all = after["AgentBehavior"]
        .as_array()
        .context("configured behaviors missing")?;
    for original in old {
        ensure!(
            all.iter().any(|row| row == original),
            "Engineer or existing behavior was changed"
        );
    }
    for collection in ["AgentContext", "Tools"] {
        for original in before[collection]
            .as_array()
            .context("baseline components missing")?
        {
            ensure!(
                after[collection]
                    .as_array()
                    .context("configured components missing")?
                    .contains(original),
                "existing {collection} was changed"
            );
        }
    }
    let added = all
        .iter()
        .filter(|row| {
            !old.iter()
                .any(|prior| prior["behavior_id"] == row["behavior_id"])
        })
        .collect::<Vec<_>>();
    ensure!(
        added.len() == 1,
        "expected one new monitoring behavior, got {}",
        added.len()
    );
    let behavior = added[0];
    ensure!(behavior["enabled"] == true, "monitor disabled");
    let context = after["AgentContext"]
        .as_array()
        .context("contexts missing")?
        .iter()
        .find(|row| row["context_id"] == behavior["context_id"])
        .context("monitor context missing")?;
    let tools = after["Tools"]
        .as_array()
        .context("tools missing")?
        .iter()
        .find(|row| row["tools_id"] == context["tools_id"])
        .context("monitor tools missing")?;
    let mut document = tools.clone();
    document
        .as_object_mut()
        .context("invalid tools document")?
        .remove("_docID");
    let (_, projected) =
        gents::config_client::config_projection(gents::Collection::Tools, Some(&document))?;
    let tools: gents::document_config::Tools =
        serde_json::from_value(projected.context("tools projection missing")?)?;
    let host = tools.host.context("monitor host tools missing")?;
    ensure!(
        host.bash
            .is_some_and(|bash| bash.mode == gents::tool_surface::BashMode::ReadOnly),
        "monitor bash must stay read-only"
    );
    ensure!(
        host.files
            .is_some_and(|files| files.mode == gents::tool_surface::FileToolMode::ReadOnly),
        "monitor files must stay read-only"
    );
    let tasks = after["Task"]
        .as_array()
        .context("tasks missing")?
        .iter()
        .filter(|row| row["behavior_id"] == behavior["behavior_id"] && row["enabled"] == true)
        .collect::<Vec<_>>();
    ensure!(tasks.len() == 1, "expected one enabled monitoring task");
    let triggers = after["Trigger"]
        .as_array()
        .context("triggers missing")?
        .iter()
        .filter(|row| row["task_id"] == tasks[0]["task_id"] && row["enabled"] == true)
        .collect::<Vec<_>>();
    ensure!(
        triggers.iter().any(
            |trigger| after["Schedule"].as_array().is_some_and(|rows| rows
                .iter()
                .any(|row| row["schedule_id"] == trigger["source"]["schedule_id"]
                    && !row["schedule_id"].is_null()))
        ),
        "monitor has no linked schedule trigger"
    );
    ensure!(
        triggers.iter().any(
            |trigger| after["EventSource"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| row["event_source_id"]
                    == trigger["source"]["event_source_id"]
                    && !row["event_source_id"].is_null()))
        ),
        "monitor has no linked document trigger"
    );
    Ok(behavior["behavior_id"]
        .as_str()
        .context("monitor ID missing")?
        .into())
}
