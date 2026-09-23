//! The monitor-findings eval definition (M3): its pack installs twenty valid
//! cases whose every check is a builtin, and a case sidecar the manifest does
//! not declare is refused; and two of those cases grade scripted mailbox rows
//! into exact verdict rows.

mod support;

use std::collections::BTreeMap;
use std::path::PathBuf;

use gents::config_client::read_desired_state_record_in_txn;
use gents::document_config::{EvalCapture, EvalDefinition, EvalSplit, EvalTier};
use gents::eval::checks::CheckRegistry;
use gents::eval::runner::embedded::EmbeddedHome;
use gents::eval::runner::{
    run, CaptureResult, CellRequest, CellSource, RunOptions, RunRequest, ScriptKey,
    ScriptedExecutor, StageEvidence, TrialEvidence, TrialLocator,
};
use gents::eval::{load_trials, load_verdicts, Anchor, OutcomeKind, TrialUsage};
use gents::pack::{install_pack_documents, load_pack_config, resolve_pack, PackInstallOptions};
use gents::{Collection, ConfigAccess};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

const DEFINITION_ID: &str = "monitor-findings";

/// Check refs across all thirty stages, as converted. The fix pass adds two.
const CHECK_REFS: usize = 188;

const CASE_IDS: [&str; 20] = [
    "ambiguous-empty-message",
    "ambiguous-number-no-condition",
    "ambiguous-omitted-condition",
    "concurrent-grouping-by-correlation",
    "concurrent-no-cross-talk",
    "concurrent-two-correlations",
    "dedup-new-correlation-same-content",
    "dedup-same-correlation-new-content",
    "dedup-same-correlation-resubmitted",
    "file-findings-from-inventory",
    "file-with-contradicting-input",
    "file-without-contradiction-multi",
    "numeric-threshold-stated-normal",
    "recovery-clearance-never-reported",
    "recovery-condition-cleared",
    "recovery-partial-clearance",
    "service-name-only",
    "single-disk-warning",
    "three-conditions-with-service",
    "two-conditions-disk-docker",
];

/// A launching home with the definition pack installed through the shared
/// pack loader, the way `gents pack install` publishes a documents pack.
struct Launching {
    _home: EmbeddedHome,
    access: ConfigAccess,
    dirs: TempDir,
    owner: String,
}

impl Launching {
    async fn new() -> Self {
        let home = EmbeddedHome::create_temp("eval-monitor-findings")
            .await
            .unwrap();
        let access = ConfigAccess::Local(home.node.clone());
        let owner = home.did().to_string();
        gents::ensure_agent_principal(home.node.as_ref(), &owner)
            .await
            .unwrap();
        let config = resolve_pack("eval_monitor_findings")
            .unwrap()
            .load_config(&PackInstallOptions {
                agent_did: owner.clone(),
            })
            .unwrap();
        install_pack_documents(&access, &config).await.unwrap();
        Self {
            _home: home,
            access,
            dirs: tempfile::tempdir().unwrap(),
            owner,
        }
    }

    async fn definition(&self) -> EvalDefinition {
        let owner = self.owner.as_str();
        let (_, value) = self
            .access
            .transact("eval_monitor_findings.read_definition", |txn| {
                Box::pin(async move {
                    read_desired_state_record_in_txn(
                        txn,
                        Collection::EvalDefinition,
                        owner,
                        DEFINITION_ID,
                    )
                    .await
                })
            })
            .await
            .unwrap()
            .expect("the installed definition");
        serde_json::from_value(value).unwrap()
    }

    async fn install(&self, documents: Vec<(Collection, Value)>) {
        support::desired_state::install_documents(
            &self.access,
            "eval_monitor_findings.install",
            documents,
        )
        .await;
    }

    /// The `monitor` profile every cell of these runs binds its slot to.
    async fn install_inference(&self, endpoint: &str, auth: Value, model: &str) {
        self.install(vec![
            (
                Collection::InferenceBackend,
                json!({
                    "agent_did": self.owner,
                    "backend_id": "monitor-backend",
                    "name": "Monitor findings backend",
                    "provider_kind": "OpenAiCompatible",
                    "openai_wire_api": "chat_completions",
                    "endpoint": endpoint,
                    "auth": auth,
                    "max_concurrent": 4,
                    "max_queue_depth": 100,
                }),
            ),
            (
                Collection::InferenceSampling,
                json!({"agent_did": self.owner, "sampling_id": "monitor-sampling", "temperature": 0.0}),
            ),
            (
                Collection::InferenceProfile,
                json!({
                    "agent_did": self.owner,
                    "profile_id": "monitor",
                    "backend_id": "monitor-backend",
                    "model_name": model,
                    "sampling_id": "monitor-sampling",
                }),
            ),
        ])
        .await;
    }

    fn runs_dir(&self) -> PathBuf {
        self.dirs.path().join("eval/runs")
    }

    /// One cell: the golden monitor, bound to the `monitor` profile.
    fn request(&self, run_id: &str, split: EvalSplit) -> RunRequest {
        RunRequest {
            run_id: run_id.into(),
            owner: self.owner.clone(),
            evaluator_did: self.owner.clone(),
            definition_id: DEFINITION_ID.into(),
            split,
            case_ids: None,
            cells: vec![CellRequest {
                cell_id: "baseline".into(),
                label: "baseline".into(),
                source: CellSource::InstalledPack {
                    name: "eval_monitor".into(),
                },
                behavior_id: "eval-monitor".into(),
                inference_profile_id: "monitor".into(),
            }],
            trials_per_case: 1,
            seed_base: 1000,
            deadline_secs: None,
            concurrency: 1,
            max_infra_retries: 0,
            breaker_threshold: 10,
            purpose: "eval".into(),
            source_commit: "m3-definition".into(),
            source_dirty: false,
            captures: Vec::new(),
            runs_dir: self.runs_dir(),
        }
    }
}

#[tokio::test]
async fn the_definition_pack_installs_twenty_valid_cases_whose_checks_are_all_registered() {
    let launching = Launching::new().await;
    let definition = launching.definition().await;
    definition.validate().unwrap();
    assert_eq!(
        (
            definition.definition_id.as_str(),
            definition.comparability_version
        ),
        (DEFINITION_ID, 1)
    );
    let mut ids: Vec<&str> = definition
        .cases
        .iter()
        .map(|case| case.case_id.as_str())
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, CASE_IDS);
    let on = |split| {
        definition
            .cases
            .iter()
            .filter(|case| case.split == split)
            .count()
    };
    // 8/6/6: held-out confirmation needs at least six cases (CHECKS.md, "Splits").
    assert_eq!(
        (
            on(EvalSplit::Train),
            on(EvalSplit::Validation),
            on(EvalSplit::HeldOut)
        ),
        (8, 6, 6)
    );

    let registry = CheckRegistry::builtin();
    let items = EvalCapture::Documents {
        name: "items".into(),
        collection: "MailboxItem".into(),
        filter: json!({"requester_did": {"_eq": "$trial"}}),
        fields: vec![
            "title".into(),
            "summary".into(),
            "payload".into(),
            "status".into(),
        ],
    };
    let (mut stages, mut refs) = (0, 0);
    for case in &definition.cases {
        for stage in &case.stages {
            stages += 1;
            assert_eq!(
                stage.capture,
                [items.clone()],
                "{} {}",
                case.case_id,
                stage.stage_id
            );
            for check in &stage.checks {
                refs += 1;
                assert!(
                    registry.get(&check.check).is_some(),
                    "{} {} names unregistered {}",
                    case.case_id,
                    stage.stage_id,
                    check.check
                );
                assert_eq!((check.tier, check.weight), (EvalTier::Acceptance, 1));
            }
        }
    }
    assert_eq!((stages, refs), (30, CHECK_REFS));
}

#[test]
fn the_definition_pack_refuses_a_case_sidecar_its_manifest_does_not_declare() {
    let pack = resolve_pack("eval_monitor_findings").unwrap();
    let mut manifest = pack.manifest.clone();
    manifest
        .metadata
        .assets
        .retain(|asset| asset != "cases/single_disk_warning.json");
    let error = load_pack_config(
        &manifest,
        &PackInstallOptions {
            agent_did: "did:key:zMonitorFindingsOwner".into(),
        },
        &|path| Ok(pack.asset(path)?.to_vec()),
        &|_| None,
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("undeclared pack sidecar: cases/single_disk_warning.json"),
        "{error:#}"
    );
}

fn key(case_id: &str) -> ScriptKey {
    ScriptKey {
        cell_label: "baseline".into(),
        case_id: case_id.into(),
        trial_index: 0,
        attempt: 1,
    }
}

/// A completed stage whose `items` capture holds `rows`.
fn completed(stage_id: &str, rows: Vec<Value>) -> StageEvidence {
    StageEvidence {
        stage_id: stage_id.into(),
        request_id: Some(format!("request-{stage_id}")),
        terminal_state: Some(RequestLifecycleState::Completed),
        failure_kind: None,
        provider_reason: None,
        messages: Vec::new(),
        tool_calls: Vec::new(),
        inference_calls: Vec::new(),
        captures: BTreeMap::from([("items".to_string(), CaptureResult::Documents { rows })]),
    }
}

fn evidence(stages: Vec<StageEvidence>) -> TrialEvidence {
    let anchor = Anchor {
        terminal_states: stages
            .iter()
            .filter_map(|stage| stage.terminal_state)
            .collect(),
        requests: stages.len() as u32,
        inference_calls: 0,
    };
    TrialEvidence::new(
        TrialLocator {
            trial_agent_did: "did:key:zScriptedMonitorTrial".into(),
            session_id: "session-scripted".into(),
            home_hint: None,
        },
        stages,
        TrialUsage::default(),
        anchor,
    )
}

/// The one open mailbox row the subject maintains, holding `findings`.
fn mailbox_row(summary: &str, findings: Value) -> Value {
    json!({
        "_docID": "bae-scripted-row",
        "title": "Monitor findings",
        "summary": summary,
        "status": "open",
        "payload": json!({"version": 1, "findings": findings}).to_string(),
    })
}

type Row = (String, String, String, OutcomeKind, Option<u32>, String);

fn expected(case_id: &str, stage_id: &str, check: &str, kind: OutcomeKind, reason: &str) -> Row {
    let score = if kind == OutcomeKind::Passed {
        Some(10_000)
    } else {
        Some(0)
    };
    (
        case_id.into(),
        stage_id.into(),
        check.into(),
        kind,
        score,
        reason.into(),
    )
}

/// Two converted cases grade scripted mailbox rows into exactly the verdict
/// rows their checks promise, without a model: one clean single-stage case,
/// and a two-stage recovery case whose second stage forgets to resolve.
#[tokio::test]
async fn two_cases_grade_scripted_mailbox_rows_into_the_exact_verdict_rows() {
    let launching = Launching::new().await;
    launching
        .install_inference(
            "http://127.0.0.1:1/v1",
            json!({"kind": "unauthenticated"}),
            "scripted-model",
        )
        .await;
    let mut request = launching.request("run-scripted", EvalSplit::Train);
    request.case_ids = Some(vec![
        "single-disk-warning".into(),
        "recovery-condition-cleared".into(),
    ]);
    let disk = mailbox_row(
        "Disk usage high on archive-02 /var (88%)",
        json!([{"correlation": "c-01-a", "condition": "disk_usage_high", "state": "open", "detail": "/var at 88% and rising"}]),
    );
    let lagging = mailbox_row(
        "Replication lag on ledger-db standby",
        json!([{"correlation": "c-09-a", "condition": "replication_lag", "state": "open", "detail": "standby lag 42 minutes and widening"}]),
    );
    let never_resolved = mailbox_row(
        "Replication lag on ledger-db standby",
        json!([{"correlation": "c-09-a", "condition": "replication_lag", "state": "open", "detail": "standby caught up; lag under a second"}]),
    );
    let executor = ScriptedExecutor::new()
        .with(
            key("single-disk-warning"),
            evidence(vec![completed("report", vec![disk])]),
        )
        .with(
            key("recovery-condition-cleared"),
            evidence(vec![
                completed("report", vec![lagging]),
                completed("update", vec![never_resolved]),
            ]),
        );

    let outcome = run(
        &launching.access,
        &request,
        &executor,
        &CheckRegistry::builtin(),
        CancellationToken::new(),
        &RunOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        (
            outcome.completed,
            outcome.not_evidence,
            outcome.breaker_tripped
        ),
        (2, 0, false)
    );

    let trials = load_trials(&launching.access, &launching.owner, "run-scripted")
        .await
        .unwrap();
    assert!(
        trials.iter().all(|trial| trial
            .completion
            .as_ref()
            .and_then(|completion| completion.evidence_digest.as_deref())
            .is_some_and(|digest| digest.len() == 64)),
        "{trials:#?}"
    );
    let case_of: BTreeMap<String, String> = trials
        .iter()
        .map(|trial| {
            (
                trial.identity.trial_id.clone(),
                trial.identity.case_id.clone(),
            )
        })
        .collect();
    let verdicts = load_verdicts(&launching.access, &launching.owner, "run-scripted")
        .await
        .unwrap();
    let mut rows: Vec<Row> = verdicts
        .iter()
        .map(|verdict| {
            (
                case_of[&verdict.trial_id].clone(),
                verdict.stage_id.clone(),
                verdict.check.clone(),
                verdict.kind,
                verdict.score_bp,
                verdict.raw["reason_code"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            )
        })
        .collect();
    rows.sort_by(|left, right| (&left.0, &left.1, &left.2).cmp(&(&right.0, &right.1, &right.2)));

    use OutcomeKind::{ModelAcceptance, Passed};
    let recovery = "recovery-condition-cleared";
    let single = "single-disk-warning";
    assert_eq!(
        rows,
        vec![
            expected(recovery, "report", "finding_count", Passed, "count_matches"),
            expected(recovery, "report", "finding_pairs_unique", Passed, "unique"),
            expected(
                recovery,
                "report",
                "finding_state_counts",
                Passed,
                "counts_match"
            ),
            expected(recovery, "report", "findings_match", Passed, "matched"),
            expected(
                recovery,
                "report",
                "mailbox_item_count",
                Passed,
                "count_matches"
            ),
            expected(
                recovery,
                "report",
                "mailbox_text_excludes",
                Passed,
                "absent"
            ),
            expected(
                recovery,
                "report",
                "payload_well_formed",
                Passed,
                "well_formed"
            ),
            expected(recovery, "update", "finding_count", Passed, "count_matches"),
            expected(recovery, "update", "finding_pairs_unique", Passed, "unique"),
            expected(
                recovery,
                "update",
                "finding_state_counts",
                ModelAcceptance,
                "counts_differ"
            ),
            expected(
                recovery,
                "update",
                "findings_match",
                ModelAcceptance,
                "unmatched_matcher"
            ),
            expected(
                recovery,
                "update",
                "mailbox_item_count",
                Passed,
                "count_matches"
            ),
            expected(
                recovery,
                "update",
                "mailbox_text_excludes",
                Passed,
                "absent"
            ),
            expected(
                recovery,
                "update",
                "payload_well_formed",
                Passed,
                "well_formed"
            ),
            expected(single, "report", "finding_count", Passed, "count_matches"),
            expected(single, "report", "finding_pairs_unique", Passed, "unique"),
            expected(single, "report", "findings_match", Passed, "matched"),
            expected(
                single,
                "report",
                "mailbox_item_count",
                Passed,
                "count_matches"
            ),
            expected(single, "report", "mailbox_text_excludes", Passed, "absent"),
            expected(
                single,
                "report",
                "payload_well_formed",
                Passed,
                "well_formed"
            ),
        ]
    );
}
