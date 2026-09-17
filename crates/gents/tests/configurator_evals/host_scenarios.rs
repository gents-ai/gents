use super::{
    host::{configuration_snapshot, input_document_id, Host},
    reporting,
    stages::{self, CaseId},
};
use anyhow::{ensure, Context, Result};
use serde_json::Value;
use std::path::Path;

#[path = "host_candidates.rs"]
mod candidates;

pub(super) const CASES: &[CaseId] = &[
    CaseId::new("host-preview"),
    CaseId::new("host-configure"),
    CaseId::new("host-healthy"),
    CaseId::new("host-findings"),
    CaseId::new("host-deduplicate"),
    CaseId::new("host-recovery"),
    CaseId::new("host-restart"),
    CaseId::new("host-schedule"),
    CaseId::new("host-improvement"),
    CaseId::new("host-regression-rejected"),
];
const PREVIEW: &str = include_str!("../fixtures/configurator_evals/host/steward.md");
const APPROVE: &str = include_str!("../fixtures/configurator_evals/host/approve-steward.md");

pub(super) fn provenance() -> Result<reporting::RunProvenance> {
    reporting::RunProvenance::current(
        "host-steward",
        "host-observations-v6-isolated-candidates",
        std::env::var("GENTS_D4F_ENDPOINT")?,
        "engineer-eval-sampling",
        1.0,
        0.95,
        &[
            reporting::EvidenceSource::new("grader", include_bytes!("host_scenarios.rs")),
            reporting::EvidenceSource::new("host", include_bytes!("host.rs")),
            reporting::EvidenceSource::new("candidates", include_bytes!("host_candidates.rs")),
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

fn verify_prompt_only_candidate(before: &Value, after: &Value, monitor: &str) -> Result<Value> {
    for collection in gents::Collection::ALL {
        let name = collection.graphql_type();
        ensure!(
            before[name].is_array() && after[name].is_array(),
            "candidate snapshot missing {name}"
        );
    }
    let behaviors = before["AgentBehavior"]
        .as_array()
        .context("behaviors missing")?;
    let selected: Vec<_> = behaviors
        .iter()
        .filter(|row| row["behavior_id"] == monitor)
        .collect();
    ensure!(
        selected.len() == 1,
        "candidate requires exactly one existing monitor"
    );
    let context_id = selected[0]["context_id"]
        .as_str()
        .context("monitor context missing")?;
    ensure!(!context_id.is_empty(), "monitor context is empty");
    ensure!(
        behaviors
            .iter()
            .filter(|row| row["context_id"] == context_id)
            .count()
            == 1,
        "candidate context is shared with another behavior"
    );
    let select = |snapshot: &Value| -> Result<(usize, Value)> {
        let contexts = snapshot["AgentContext"]
            .as_array()
            .context("contexts missing")?;
        let rows: Vec<_> = contexts
            .iter()
            .enumerate()
            .filter(|(_, row)| row["context_id"] == context_id)
            .collect();
        ensure!(
            rows.len() == 1,
            "candidate requires exactly one existing monitor context"
        );
        Ok((rows[0].0, rows[0].1.clone()))
    };
    let (_, original) = select(before)?;
    let (index, candidate) = select(after)?;
    let prompt = candidate["system_prompt"]
        .as_str()
        .context("candidate prompt missing")?;
    ensure!(!prompt.trim().is_empty(), "candidate prompt is empty");
    ensure!(
        candidate["system_prompt"] != original["system_prompt"],
        "candidate did not change the prompt"
    );
    let mut restored = after.clone();
    restored["AgentContext"][index]["system_prompt"] = original["system_prompt"].clone();
    ensure!(
        &restored == before,
        "candidate changed configuration outside the monitor prompt"
    );
    Ok(candidate)
}

#[test]
fn improvement_scope_requires_an_in_place_unshared_prompt_and_complete_snapshot() {
    let mut before = serde_json::Map::new();
    for collection in gents::Collection::ALL {
        before.insert(collection.graphql_type().into(), serde_json::json!([]));
    }
    let mut before = Value::Object(before);
    before["AgentBehavior"] = serde_json::json!([{"behavior_id":"monitor","context_id":"context"}]);
    before["AgentContext"] = serde_json::json!([{"_docID":"original", "context_id":"context","system_prompt":"original prompt","tools_id":"readonly"}]);
    let mut after = before.clone();
    after["AgentContext"][0]["system_prompt"] = "improved prompt".into();
    assert!(verify_prompt_only_candidate(&before, &after, "monitor").is_ok());
    assert!(verify_prompt_only_candidate(&before, &before, "monitor").is_err());
    for (field, value) in [
        ("system_prompt", " "),
        ("_docID", "clone"),
        ("tools_id", "writer"),
    ] {
        let mut invalid = after.clone();
        invalid["AgentContext"][0][field] = value.into();
        assert!(
            verify_prompt_only_candidate(&before, &invalid, "monitor").is_err(),
            "{field}"
        );
    }
    for collection in gents::Collection::ALL {
        let mut invalid = after.clone();
        invalid
            .as_object_mut()
            .unwrap()
            .remove(collection.graphql_type());
        assert!(verify_prompt_only_candidate(&before, &invalid, "monitor").is_err());
    }
    let mut shared = before.clone();
    shared["AgentBehavior"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"behavior_id":"other","context_id":"context"}));
    let mut shared_after = shared.clone();
    shared_after["AgentContext"][0]["system_prompt"] = "improved prompt".into();
    assert!(verify_prompt_only_candidate(&shared, &shared_after, "monitor").is_err());
    let mut extra = after.clone();
    extra["AgentContext"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"context_id":"extra"}));
    assert!(verify_prompt_only_candidate(&before, &extra, "monitor").is_err());
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
        for (case, regression) in [(CASES[8], false), (CASES[9], true)] {
            stages::checked(case, &evidence, stages::acceptance(
                candidates::evaluate(&mut host, &evidence, &engineer, &monitor, collection, regression)
            )).await?;
        }
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
    verify_finding_coverage_at(items, actual, now, 80)
}

fn verify_finding_coverage_at(
    items: &[&Value],
    actual: &Value,
    now: i64,
    disk_threshold: i64,
) -> Result<()> {
    use std::collections::BTreeSet;
    let mut expected = BTreeSet::new();
    if actual["disk_used_percent"]
        .as_i64()
        .context("disk measurement missing")?
        >= disk_threshold
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
    let tools: gents::document_config::Tools =
        decode_configuration(gents::Collection::Tools, tools)?;
    verify_monitor_authority(&tools)?;
    let surfaces = after["DatastoreToolSurface"]
        .as_array()
        .context("datastore surfaces missing")?
        .iter()
        .map(|row| decode_configuration(gents::Collection::DatastoreToolSurface, row))
        .collect::<Result<Vec<gents::document_config::DatastoreToolSurfaceDocument>>>()?;
    let selected = gents::document_config::merge_datastore_tool_surfaces(&tools, &surfaces)?;
    ensure!(
        selected
            .write_tools
            .iter()
            .all(|tool| matches!(tool.collection.as_str(), "HostObservation" | "MailboxItem")),
        "monitor may write only observations and canonical mailbox items"
    );
    let tasks = after["Task"]
        .as_array()
        .context("tasks missing")?
        .iter()
        .filter(|row| row["behavior_id"] == behavior["behavior_id"] && row["enabled"] == true)
        .collect::<Vec<_>>();
    ensure!(tasks.len() == 1, "expected one enabled monitoring task");
    let task: gents::document_config::Task =
        decode_configuration(gents::Collection::Task, tasks[0])?;
    ensure!(
        task.hooks.is_empty(),
        "read-only monitor must not install host command hooks"
    );
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

fn decode_configuration<T: serde::de::DeserializeOwned>(
    collection: gents::Collection,
    row: &Value,
) -> Result<T> {
    let mut document = row.clone();
    document
        .as_object_mut()
        .context("configuration must be an object")?
        .remove("_docID");
    let (_, projected) = gents::config_client::config_projection(collection, Some(&document))?;
    Ok(serde_json::from_value(
        projected.context("configuration projection missing")?,
    )?)
}

fn verify_monitor_authority(tools: &gents::document_config::Tools) -> Result<()> {
    let host = tools.host.as_ref().context("monitor host tools missing")?;
    let bash = host.bash.as_ref().context("monitor bash missing")?;
    ensure!(
        bash.mode == gents::tool_surface::BashMode::ReadOnly
            && bash
                .execution_mode
                .is_none_or(|mode| mode == gents::toolset::CommandExecutionMode::ReadOnly),
        "monitor bash must stay read-only"
    );
    ensure!(
        host.files.as_ref().is_none_or(|files| matches!(
            files.mode,
            gents::tool_surface::FileToolMode::Off | gents::tool_surface::FileToolMode::ReadOnly
        )),
        "monitor files must stay read-only"
    );
    let baseline = gents::toolset::default_read_only_command_policy();
    ensure!(
        bash.read_only_commands
            .iter()
            .flatten()
            .all(|command| baseline.read_only_allowlist().contains(command))
            && bash
                .allowed_argv_prefixes
                .iter()
                .flatten()
                .all(|prefix| prefix
                    .first()
                    .is_some_and(|command| baseline.read_only_allowlist().contains(command))),
        "monitor command overrides must not extend the canonical read-only allowlist"
    );
    ensure!(
        host.cli.is_empty(),
        "monitor must not select additional host-registered executors"
    );
    ensure!(
        tools.remote.as_ref().is_none_or(|remote| remote
            .services
            .iter()
            .all(|service| service.tool_names.is_empty())),
        "monitor must not select remote tools"
    );
    ensure!(
        tools
            .subagents
            .as_ref()
            .is_none_or(|subagents| subagents.target_ids.is_empty()
                && subagents.spawn_enabled != Some(true)
                && subagents.steering_enabled != Some(true)),
        "monitor must not gain delegated execution authority"
    );
    ensure!(
        tools
            .integrations
            .as_ref()
            .is_none_or(|integrations| integrations.lsp.is_none()
                && integrations.eth_tool_ids.as_ref().is_none_or(Vec::is_empty)),
        "monitor must not select external execution integrations"
    );
    ensure!(
        tools
            .self_config
            .as_ref()
            .is_none_or(|config| config.enable_self_config != Some(true)
                && config.enable_pack_install != Some(true)),
        "monitor must not retain configuration write authority"
    );
    ensure!(
        tools
            .built_ins
            .as_ref()
            .is_none_or(|built_ins| built_ins.enable_graph_tools != Some(true)
                && built_ins.enable_memory != Some(true)),
        "monitor must not gain graph execution or unrelated datastore writes"
    );
    Ok(())
}

#[test]
fn read_only_modes_do_not_hide_additional_monitor_authority() {
    use gents::document_config::{BashTools, FileTools, HostTools, Tools};
    let tools = Tools {
        host: Some(HostTools {
            files: Some(FileTools {
                mode: gents::tool_surface::FileToolMode::ReadOnly,
                ..Default::default()
            }),
            bash: Some(BashTools {
                mode: gents::tool_surface::BashMode::ReadOnly,
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(verify_monitor_authority(&tools).is_ok());
    for (group, value) in [
        (
            "self_config",
            serde_json::json!({"enable_self_config":true}),
        ),
        (
            "self_config",
            serde_json::json!({"enable_pack_install":true}),
        ),
        ("built_ins", serde_json::json!({"enable_graph_tools":true})),
        ("integrations", serde_json::json!({"lsp":{}})),
        (
            "subagents",
            serde_json::json!({"target_ids":["repair"],"spawn_enabled":true}),
        ),
        (
            "remote",
            serde_json::json!({"services":[{"mcp_service_id":"repair","tool_names":["execute"]}]}),
        ),
    ] {
        let mut modified = serde_json::to_value(&tools).unwrap();
        modified[group] = value;
        let modified: Tools = serde_json::from_value(modified).unwrap();
        assert!(
            verify_monitor_authority(&modified).is_err(),
            "accepted {group}"
        );
    }
    let mut narrowed = tools.clone();
    narrowed.host.as_mut().unwrap().files = None;
    narrowed
        .host
        .as_mut()
        .unwrap()
        .bash
        .as_mut()
        .unwrap()
        .read_only_commands = Some(vec!["df".into(), "cmp".into()]);
    assert!(verify_monitor_authority(&narrowed).is_ok());
    narrowed
        .host
        .as_mut()
        .unwrap()
        .bash
        .as_mut()
        .unwrap()
        .read_only_commands = Some(vec!["chmod".into()]);
    assert!(verify_monitor_authority(&narrowed).is_err());
    let mut cli = tools.clone();
    cli.host
        .as_mut()
        .unwrap()
        .cli
        .push(gents::document_config::CliTool {
            name: "repair".into(),
            ..Default::default()
        });
    assert!(verify_monitor_authority(&cli).is_err());
    let mut extended = tools.clone();
    extended
        .host
        .as_mut()
        .unwrap()
        .bash
        .as_mut()
        .unwrap()
        .allowed_argv_prefixes = Some(vec![vec!["chmod".into()]]);
    assert!(verify_monitor_authority(&extended).is_err());
    let mut disabled = tools;
    disabled.self_config = Some(gents::document_config::SelfConfigTools {
        enable_self_config: Some(false),
        ..Default::default()
    });
    assert!(verify_monitor_authority(&disabled).is_ok());
}

#[test]
fn monitor_configuration_rejects_hooks_and_unrelated_datastore_writers() {
    let before = serde_json::json!({
        "AgentPrincipal":[], "InferenceBackend":[], "InferenceProfile":[],
        "InferenceSampling":[], "OAuthCredential":[], "AgentBehavior":[],
        "AgentContext":[], "Tools":[]
    });
    let mut after = before.clone();
    after["AgentBehavior"] =
        serde_json::json!([{"behavior_id":"monitor","context_id":"context","enabled":true}]);
    after["AgentContext"] = serde_json::json!([{"context_id":"context","tools_id":"tools"}]);
    after["Tools"] = serde_json::json!([{"tools_id":"tools","agent_did":"did:key:owner",
        "host":{"bash":{"mode":"ReadOnly"}}}]);
    after["DatastoreToolSurface"] = serde_json::json!([]);
    after["Task"] = serde_json::json!([{"task_id":"check","agent_did":"did:key:owner",
        "behavior_id":"monitor","prompt_template":"Check this host","enabled":true}]);
    after["Schedule"] = serde_json::json!([{"schedule_id":"scheduled"}]);
    after["EventSource"] = serde_json::json!([{"event_source_id":"immediate"}]);
    after["Trigger"] = serde_json::json!([
        {"task_id":"check","enabled":true,"source":{"schedule_id":"scheduled"}},
        {"task_id":"check","enabled":true,"source":{"event_source_id":"immediate"}}
    ]);
    assert!(verify_steward_configuration(&before, &after).is_ok());
    let mut hooked = after.clone();
    hooked["Task"][0]["hooks"] = serde_json::json!([
        {"hook_id":"repair","phase":"before","command":["chmod","700","/host/api-work"]}
    ]);
    let error = verify_steward_configuration(&before, &hooked).unwrap_err();
    assert!(
        error.to_string().contains("host command hooks"),
        "{error:#}"
    );
    after["Tools"][0]["datastore"] = serde_json::json!({"datastore_tool_surface_ids":["extra"]});
    after["DatastoreToolSurface"] = serde_json::json!([{
        "surface_id":"extra","agent_did":"did:key:owner","entries":[{
            "tool_name":"write_other","collection":"OtherData","description":"Unrelated writer",
            "fields":[{"name":"value","required":true}]
        }]
    }]);
    let error = verify_steward_configuration(&before, &after).unwrap_err();
    assert!(error.to_string().contains("only observations"), "{error:#}");
}
