use anyhow::Context;
use serde_json::{json, Value};
use tempfile::TempDir;

use super::snapshot::DEFINITION_FILE;
use super::*;
use crate::config_client::{
    apply_desired_state_plan, read_desired_state_record_in_txn, DesiredStateApplyDocument,
    DesiredStateApplyPlan,
};
use crate::documents::{ensure_node, EvalSplit};
use crate::eval::invalidate_run;
use crate::eval::runner::embedded::EmbeddedHome;
use crate::eval::scoring::PILOT_PURPOSE;

pub(crate) const OWNER: &str = "did:key:eval-owner";

pub(crate) struct Launching {
    home: EmbeddedHome,
    pub(crate) access: ConfigAccess,
    dirs: TempDir,
}

impl Launching {
    pub(crate) async fn new() -> Self {
        let home = EmbeddedHome::create_temp("freeze").await.unwrap();
        let access = ConfigAccess::local(home.node.clone());
        ensure_node(&home.node, OWNER).await.unwrap();
        let launching = Self {
            home,
            access,
            dirs: tempfile::tempdir().unwrap(),
        };
        launching
            .install(vec![
                (Collection::EvalDefinition, definition()),
                (
                    Collection::InferenceBackend,
                    backend(
                        "backend",
                        "OpenAiCompatible",
                        json!({"kind": "unauthenticated"}),
                    ),
                ),
                (
                    Collection::InferenceSampling,
                    json!({"node_did": OWNER, "sampling_id": "sampling", "temperature": 0.0}),
                ),
                (
                    Collection::InferenceProfile,
                    profile("local", "backend", Some("sampling")),
                ),
            ])
            .await;
        launching
    }

    pub(crate) async fn install(&self, documents: Vec<(Collection, Value)>) {
        let plan = DesiredStateApplyPlan::new(
            documents
                .into_iter()
                .map(|(collection, value)| DesiredStateApplyDocument {
                    collection,
                    add: value.clone(),
                    update: value,
                })
                .collect(),
        )
        .unwrap();
        self.access
            .transact_write("freeze.test_fixture", |txn| {
                let plan = &plan;
                Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
            })
            .await
            .unwrap();
    }

    pub(crate) async fn delete(&self, collection: Collection, id: &str) {
        self.access
            .transact_write("freeze.test_delete", |txn| {
                Box::pin(async move {
                    let (doc_id, _) =
                        read_desired_state_record_in_txn(txn, collection, OWNER, id)
                            .await?
                            .context("document to delete")?;
                    txn.execute(&format!(
                        r#"mutation {{ delete_{name}(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}) {{ _docID }} }}"#,
                        name = collection.graphql_type(),
                        doc_id = crate::graphql::escape_graphql_string(&doc_id),
                    ))
                    .await
                    .map(|_| ())
                    .map_err(anyhow::Error::from)
                })
            })
            .await
            .unwrap();
    }

    pub(crate) fn runs_dir(&self) -> PathBuf {
        self.dirs.path().join("eval/runs")
    }

    pub(crate) fn evaluator_did(&self) -> String {
        self.home.did().to_string()
    }

    pub(crate) fn pack(&self, name: &str, bash_mode: &str) -> PathBuf {
        let root = self.dirs.path().join(name);
        write_fixture_pack(&root, bash_mode);
        root
    }

    fn request(&self, run_id: &str, pack: &Path) -> RunRequest {
        RunRequest {
            run_id: run_id.into(),
            owner: OWNER.into(),
            evaluator_did: self.evaluator_did(),
            definition_id: "monitor-findings".into(),
            split: EvalSplit::Validation,
            case_ids: None,
            cells: vec![CellRequest {
                cell_id: "baseline".into(),
                label: "baseline".into(),
                source: CellSource::Directory(pack.to_path_buf()),
                agent_id: "monitor".into(),
                inference_profile_id: "local".into(),
            }],
            trials_per_case: 2,
            seed_base: 1000,
            deadline_secs: Some(600),
            concurrency: 1,
            max_infra_retries: 1,
            breaker_threshold: 5,
            purpose: "eval".into(),
            source_commit: "0deb7659c".into(),
            source_dirty: false,
            runs_dir: self.runs_dir(),
            captures: Vec::new(),
        }
    }
}

fn definition() -> Value {
    json!({
        "definition_id": "monitor-findings",
        "node_did": OWNER,
        "comparability_version": 1,
        "subject": {"kind": "agent", "inference_slots": ["primary"]},
        "cases": [
            case("disk-warning", "validation"),
            case("train-case", "train"),
        ],
    })
}

#[test]
fn frozen_files_preserve_complete_snapshots_for_existing_readers() {
    let dir = tempfile::tempdir().unwrap();
    let before = RunSidecar {
        breaker_threshold: 5,
        captures: Vec::new(),
    };
    let original: EvalDefinition = serde_json::from_value(definition()).unwrap();
    write_sidecar(dir.path(), &before).unwrap();
    write_frozen_definition(dir.path(), &original).unwrap();
    let sidecar_reader = std::fs::File::open(sidecar_path(dir.path())).unwrap();
    let definition_reader = std::fs::File::open(dir.path().join(DEFINITION_FILE)).unwrap();

    let after = RunSidecar {
        breaker_threshold: 10,
        ..before.clone()
    };
    let mut revised = original.clone();
    revised.comparability_version += 1;
    write_sidecar(dir.path(), &after).unwrap();
    write_frozen_definition(dir.path(), &revised).unwrap();

    assert_eq!(
        serde_json::from_reader::<_, RunSidecar>(sidecar_reader).unwrap(),
        before
    );
    assert_eq!(
        serde_json::from_reader::<_, EvalDefinition>(definition_reader).unwrap(),
        original
    );
    assert_eq!(read_sidecar(dir.path()).unwrap(), Some(after));
    assert_eq!(
        read_frozen_definition(dir.path(), &definition_ref(&revised).unwrap()).unwrap(),
        Some(revised)
    );
}

fn case(case_id: &str, split: &str) -> Value {
    json!({
        "case_id": case_id,
        "split": split,
        "stages": [{
            "stage_id": "check",
            "prompt": "Run the monitor.",
            "deadline_secs": 600,
            "checks": [{"check": "captured_rows_count", "params": {"name": "findings"}, "tier": "acceptance"}],
        }],
    })
}

fn backend(backend_id: &str, provider_kind: &str, auth: Value) -> Value {
    json!({
        "node_did": OWNER,
        "backend_id": backend_id,
        "name": "Workstation",
        "provider_kind": provider_kind,
        "endpoint": "http://127.0.0.1:8000/v1",
        "auth": auth,
    })
}

fn profile(profile_id: &str, backend_id: &str, sampling_id: Option<&str>) -> Value {
    json!({
        "node_did": OWNER,
        "profile_id": profile_id,
        "backend_id": backend_id,
        "model_name": "test-model",
        "sampling_id": sampling_id,
    })
}

pub(crate) const FIXTURE_PROMPT: &str = "Watch the mailbox.\n";

pub(crate) fn write_fixture_pack(root: &Path, bash_mode: &str) {
    std::fs::create_dir_all(root.join("agents/monitor")).unwrap();
    std::fs::write(root.join("README.md"), "# monitor fixture\n").unwrap();
    std::fs::write(root.join("agents/monitor/system_prompt.md"), FIXTURE_PROMPT).unwrap();
    let manifest = json!({
        "manifest_version": 1,
        "name": "monitor_fixture",
        "version": "1.0.0",
        "description": "Freeze fixture: one monitor behavior.",
        "authors": ["gents-ai contributors"],
        "kind": "documents",
        "assets": [
            "README.md",
            "agents/monitor/system_prompt.md",
            "pack_config.json",
        ],
        "config": "pack_config.json",
        "inference_slots": [{
            "name": "primary",
            "description": "Runs the monitor behavior.",
            "agents": ["monitor"],
        }],
    });
    std::fs::write(
        root.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let config = json!({
        "node": {},
        "agents": [{
            "agent_id": "monitor",
            "display_name": "Monitor",
            "context_id": "monitor-context",
            "inference_profile_id": "gents:inference-slot:primary",
        }],
        "contexts": [{
            "context_id": "monitor-context",
            "display_name": "Monitor",
            "system_prompt": "./agents/monitor/system_prompt.md",
            "tools_id": "monitor-tools",
        }],
        "tools": [{
            "tools_id": "monitor-tools",
            "display_name": "Monitor tools",
            "host": {"bash": {"mode": bash_mode}},
        }],
    });
    std::fs::write(
        root.join("pack_config.json"),
        serde_json::to_vec_pretty(&config).unwrap(),
    )
    .unwrap();
}

fn refusal(error: &anyhow::Error) -> String {
    freeze_refused(error)
        .unwrap_or_else(|| panic!("expected a FreezeRefused, got {error:#}"))
        .0
        .clone()
}

fn sidecar(run_dir: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(run_dir.join("run.json")).unwrap()).unwrap()
}

#[tokio::test]
async fn freeze_writes_the_run_materializes_the_pack_and_is_idempotent() {
    let launching = Launching::new().await;
    let pack = launching.pack("pack", "Off");
    let request = launching.request("run-1", &pack);

    let frozen = freeze(&launching.access, &request, Isolation::Embedded)
        .await
        .unwrap();
    assert_eq!(frozen.record.origin.case_ids, ["disk-warning"]);
    assert_eq!(
        frozen.record.origin.denominator_policy,
        DENOMINATOR_POLICY_V1
    );
    assert_eq!(frozen.record.origin.taxonomy_version, TAXONOMY_VERSION);
    assert_eq!(
        frozen.record.origin.check_registry_version,
        CHECK_REGISTRY_VERSION
    );
    assert_eq!(frozen.definition.definition_id, "monitor-findings");

    let cell = &frozen.cells[0];
    assert_eq!(cell.spec.subject.agent_id, "monitor");
    assert!(cell.spec.subject.pack_digest.starts_with("sha256:"));
    assert_eq!(
        frozen.record.origin.cells[0].subject.pack_digest,
        cell.spec.subject.pack_digest
    );
    assert!(!cell.tools_unrestricted_bash);
    assert_eq!(cell.inference.profile["profile_id"], "local");
    assert_eq!(cell.inference.backend["backend_id"], "backend");
    assert_eq!(
        cell.inference.sampling.as_ref().expect("sampling document")["sampling_id"],
        "sampling"
    );
    assert_eq!(cell.inference.seed, 0, "the loop seeds each trial");

    assert!(cell.pack_dir.join("manifest.json").exists());
    assert!(cell
        .pack_dir
        .join("agents/monitor/system_prompt.md")
        .exists());
    assert_eq!(sidecar(&frozen.run_dir)["breaker_threshold"], 5);

    let again = freeze(&launching.access, &request, Isolation::Embedded)
        .await
        .unwrap();
    assert_eq!(again.record, frozen.record, "a frozen run is reused whole");
}

#[tokio::test]
async fn freeze_records_the_requested_evaluator_and_refuses_an_empty_one() {
    let launching = Launching::new().await;
    let pack = launching.pack("pack", "Off");
    let request = launching.request("run-1", &pack);

    let frozen = freeze(&launching.access, &request, Isolation::Embedded)
        .await
        .unwrap();
    assert_eq!(frozen.record.evaluator_did, request.evaluator_did);
    assert_ne!(
        frozen.record.evaluator_did, frozen.record.owner,
        "the launching home is not the owning principal in this fixture"
    );
    let stored = load_run(&launching.access, OWNER, "run-1")
        .await
        .unwrap()
        .expect("the frozen run");
    assert_eq!(stored.evaluator_did, request.evaluator_did);

    for empty in ["", "   "] {
        let mut anonymous = launching.request("run-2", &pack);
        anonymous.evaluator_did = empty.into();
        let error = freeze(&launching.access, &anonymous, Isolation::Embedded)
            .await
            .unwrap_err();
        assert!(refusal(&error).contains("evaluator DID"), "{error:#}");
    }
}

#[tokio::test]
async fn freeze_refuses_a_duplicate_or_unsafe_cell_id() {
    let launching = Launching::new().await;
    let pack = launching.pack("pack", "Off");

    let mut duplicate = launching.request("run-1", &pack);
    duplicate.cells.push(duplicate.cells[0].clone());
    let error = freeze(&launching.access, &duplicate, Isolation::Embedded)
        .await
        .unwrap_err();
    let reason = refusal(&error);
    assert!(
        reason.contains("baseline") && reason.contains("twice"),
        "{reason}"
    );

    let mut empty = launching.request("run-1", &pack);
    empty.cells.clear();
    let error = freeze(&launching.access, &empty, Isolation::Embedded)
        .await
        .unwrap_err();
    assert!(refusal(&error).contains("no cell"), "{error:#}");

    for escaping in ["..", "../sibling", "nested/cell", ""] {
        let mut unsafe_id = launching.request("run-1", &pack);
        unsafe_id.cells[0].cell_id = escaping.into();
        let error = freeze(&launching.access, &unsafe_id, Isolation::Embedded)
            .await
            .unwrap_err();
        let reason = refusal(&error);
        assert!(
            reason.contains("cell_id") && reason.contains("path component"),
            "{escaping:?}: {reason}"
        );
    }

    let mut unsafe_run = launching.request("../escape", &pack);
    unsafe_run.cells[0].cell_id = "baseline".into();
    let error = freeze(&launching.access, &unsafe_run, Isolation::Embedded)
        .await
        .unwrap_err();
    assert!(refusal(&error).contains("run_id"), "{error:#}");
}

#[tokio::test]
async fn freeze_materializes_only_the_declared_assets() {
    let launching = Launching::new().await;
    let pack = launching.pack("pack", "Off");
    std::fs::write(pack.join("scratch.txt"), "not declared\n").unwrap();
    std::fs::create_dir_all(pack.join(".git")).unwrap();
    std::fs::write(pack.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();

    let frozen = freeze(
        &launching.access,
        &launching.request("run-1", &pack),
        Isolation::Embedded,
    )
    .await
    .unwrap();
    let materialized = &frozen.cells[0].pack_dir;
    assert!(materialized.join("manifest.json").exists());
    assert!(materialized
        .join("agents/monitor/system_prompt.md")
        .exists());
    assert!(!materialized.join("scratch.txt").exists());
    assert!(!materialized.join(".git").exists());
    assert!(
        !materialized.with_file_name("pack.tmp").exists(),
        "the staging directory is renamed into place, never left behind"
    );
}

#[tokio::test]
async fn freeze_refuses_a_seed_range_past_i64() {
    let launching = Launching::new().await;
    let pack = launching.pack("pack", "Off");

    let mut overflowing = launching.request("run-1", &pack);
    overflowing.seed_base = i64::MAX;
    overflowing.trials_per_case = 2;
    let error = freeze(&launching.access, &overflowing, Isolation::Embedded)
        .await
        .unwrap_err();
    let reason = refusal(&error);
    assert!(
        reason.contains("seed_base") && reason.contains("trials_per_case"),
        "{reason}"
    );
    assert!(
        !launching.runs_dir().join("run-1").exists(),
        "a refused request writes nothing"
    );

    let mut fitting = launching.request("run-2", &pack);
    fitting.seed_base = i64::MAX;
    fitting.trials_per_case = 1;
    let frozen = freeze(&launching.access, &fitting, Isolation::Embedded)
        .await
        .unwrap();
    assert_eq!(frozen.record.origin.seed_base, i64::MAX);
}

#[tokio::test]
async fn freeze_refuses_a_changed_origin_for_an_existing_run_id() {
    let launching = Launching::new().await;
    let pack = launching.pack("pack", "Off");
    let request = launching.request("run-1", &pack);
    freeze(&launching.access, &request, Isolation::Embedded)
        .await
        .unwrap();

    let mut changed = request.clone();
    changed.trials_per_case = 3;
    let error = freeze(&launching.access, &changed, Isolation::Embedded)
        .await
        .unwrap_err();
    assert!(refusal(&error).contains("different origin"), "{error:#}");
}

#[tokio::test]
async fn freeze_refuses_an_unknown_case_and_a_case_off_the_split() {
    let launching = Launching::new().await;
    let pack = launching.pack("pack", "Off");

    let mut unknown = launching.request("run-1", &pack);
    unknown.case_ids = Some(vec!["nope".into()]);
    let error = freeze(&launching.access, &unknown, Isolation::Embedded)
        .await
        .unwrap_err();
    assert!(refusal(&error).contains("nope"), "{error:#}");

    let mut off_split = launching.request("run-2", &pack);
    off_split.case_ids = Some(vec!["train-case".into()]);
    let error = freeze(&launching.access, &off_split, Isolation::Embedded)
        .await
        .unwrap_err();
    let reason = refusal(&error);
    assert!(
        reason.contains("train-case") && reason.contains("split"),
        "{reason}"
    );
}

#[tokio::test]
async fn freeze_refuses_any_host_bash_under_embedded_but_not_under_process() {
    let launching = Launching::new().await;

    for (run_id, mode) in [("embedded-ro", "ReadOnly"), ("embedded-rw", "Unrestricted")] {
        let pack = launching.pack(&format!("bash-{mode}"), mode);
        let error = freeze(
            &launching.access,
            &launching.request(run_id, &pack),
            Isolation::Embedded,
        )
        .await
        .unwrap_err();
        let reason = refusal(&error);
        assert!(
            reason.contains("monitor-tools") && reason.contains(mode),
            "{mode}: {reason}"
        );
        assert!(
            !launching.runs_dir().join(run_id).exists(),
            "a refused request leaves no run directory behind: {run_id}"
        );
    }

    let off = launching.pack("pack", "Off");
    let frozen = freeze(
        &launching.access,
        &launching.request("embedded-off", &off),
        Isolation::Embedded,
    )
    .await
    .unwrap();
    assert!(!frozen.cells[0].tools_unrestricted_bash);

    let unrestricted = launching.pack("bash-Unrestricted", "Unrestricted");
    let frozen = freeze(
        &launching.access,
        &launching.request("process", &unrestricted),
        Isolation::Process,
    )
    .await
    .unwrap();
    assert!(frozen.cells[0].tools_unrestricted_bash);

    let read_only = launching.pack("bash-ReadOnly", "ReadOnly");
    let frozen = freeze(
        &launching.access,
        &launching.request("process-read-only", &read_only),
        Isolation::Process,
    )
    .await
    .unwrap();
    assert!(
        !frozen.cells[0].tools_unrestricted_bash,
        "read-only bash is host bash, but it is not unrestricted bash"
    );
}

#[tokio::test]
async fn freeze_refuses_an_oauth_backend() {
    let launching = Launching::new().await;
    launching
        .install(vec![
            (
                Collection::InferenceBackend,
                backend(
                    "subscription",
                    "ClaudeCliSubscription",
                    json!({"kind": "node_oauth"}),
                ),
            ),
            (
                Collection::InferenceProfile,
                profile("borrowed", "subscription", None),
            ),
        ])
        .await;
    let pack = launching.pack("pack", "Off");
    let mut request = launching.request("run-1", &pack);
    request.cells[0].inference_profile_id = "borrowed".into();

    let error = freeze(&launching.access, &request, Isolation::Embedded)
        .await
        .unwrap_err();
    assert!(refusal(&error).contains("node_oauth"), "{error:#}");
}

#[tokio::test]
async fn freeze_refuses_a_profile_whose_sampling_document_is_missing() {
    let launching = Launching::new().await;
    launching
        .install(vec![
            (
                Collection::InferenceSampling,
                json!({"node_did": OWNER, "sampling_id": "spare", "temperature": 1.0}),
            ),
            (
                Collection::InferenceProfile,
                profile("dangling", "backend", Some("spare")),
            ),
        ])
        .await;
    launching
        .delete(Collection::InferenceSampling, "spare")
        .await;
    let pack = launching.pack("pack", "Off");
    let mut request = launching.request("run-1", &pack);
    request.cells[0].inference_profile_id = "dangling".into();

    let error = freeze(&launching.access, &request, Isolation::Embedded)
        .await
        .unwrap_err();
    assert!(refusal(&error).contains("spare"), "{error:#}");
}

#[tokio::test]
async fn freeze_refuses_an_invalidated_run() {
    let launching = Launching::new().await;
    let pack = launching.pack("pack", "Off");
    let request = launching.request("run-1", &pack);
    let origin = RunOrigin {
        definition: crate::eval::DefinitionRef {
            definition_id: "monitor-findings".into(),
            comparability_version: 1,
            digest: "sha256:whatever".into(),
        },
        split: EvalSplit::Validation,
        case_ids: vec!["disk-warning".into()],
        cells: Vec::new(),
        trials_per_case: 2,
        seed_base: 1000,
        deadline_secs: Some(600),
        concurrency: 1,
        denominator_policy: DENOMINATOR_POLICY_V1.into(),
        taxonomy_version: TAXONOMY_VERSION.into(),
        max_infra_retries: 1,
        check_registry_version: CHECK_REGISTRY_VERSION.into(),
        source_commit: "0deb7659c".into(),
        source_dirty: false,
        purpose: "eval".into(),
        breaker_threshold: 5,
    };
    create_run(&launching.access, "run-1", OWNER, OWNER, &origin)
        .await
        .unwrap();
    invalidate_run(&launching.access, OWNER, "run-1", OWNER, "grader bug")
        .await
        .unwrap();

    let error = freeze(&launching.access, &request, Isolation::Embedded)
        .await
        .unwrap_err();
    assert!(refusal(&error).contains("invalidated"), "{error:#}");
}

#[tokio::test]
async fn freeze_refuses_a_malformed_purpose() {
    let launching = Launching::new().await;
    let pack = launching.pack("pack", "Off");
    for malformed in ["optimization:", "other"] {
        let mut request = launching.request("run-1", &pack);
        request.purpose = malformed.into();
        let error = freeze(&launching.access, &request, Isolation::Embedded)
            .await
            .unwrap_err();
        assert!(refusal(&error).contains(malformed), "{error:#}");
    }
    for (run_id, purpose) in [
        ("plain", "eval"),
        ("tuned", "optimization:job-1"),
        ("draft", PILOT_PURPOSE),
    ] {
        let mut request = launching.request(run_id, &pack);
        request.purpose = purpose.into();
        let frozen = freeze(&launching.access, &request, Isolation::Embedded)
            .await
            .unwrap();
        assert_eq!(frozen.record.origin.purpose, purpose);
    }
}

#[tokio::test]
async fn freeze_persists_request_captures_and_refuses_a_changed_list() {
    let launching = Launching::new().await;
    let pack = launching.pack("pack", "Off");
    let mut request = launching.request("run-1", &pack);
    request.captures = vec![Capture::Documents {
        name: "findings".into(),
        collection: "ExperimentFinding".into(),
        filter: json!({}),
        fields: vec!["finding_id".into()],
    }];

    let frozen = freeze(&launching.access, &request, Isolation::Embedded)
        .await
        .unwrap();
    assert_eq!(frozen.captures, request.captures);
    assert_eq!(
        sidecar(&frozen.run_dir)["captures"][0]["name"],
        "findings",
        "the capture list is frozen next to the run"
    );

    let mut changed = request.clone();
    changed.captures.clear();
    let error = freeze(&launching.access, &changed, Isolation::Embedded)
        .await
        .unwrap_err();
    assert!(refusal(&error).contains("capture"), "{error:#}");
}

#[tokio::test]
async fn freeze_writes_the_definition_it_froze_and_resume_reads_it_back() {
    let launching = Launching::new().await;
    let pack = launching.pack("pack", "Off");
    let frozen = freeze(
        &launching.access,
        &launching.request("run-def", &pack),
        Isolation::Embedded,
    )
    .await
    .unwrap();
    let path = frozen.run_dir.join(DEFINITION_FILE);
    assert!(path.is_file());
    let read = read_frozen_definition(&frozen.run_dir, &frozen.record.origin.definition)
        .unwrap()
        .expect("the file is there");
    assert_eq!(read, frozen.definition);

    let mut edited = serde_json::to_value(&frozen.definition).unwrap();
    edited["title"] = json!("edited after the freeze");
    launching
        .install(vec![(Collection::EvalDefinition, edited)])
        .await;
    let thawed = thaw(
        &launching.access,
        OWNER,
        "run-def",
        &launching.runs_dir(),
        Isolation::Embedded,
    )
    .await
    .unwrap();
    assert_eq!(thawed.definition, frozen.definition);

    let mut tampered = serde_json::to_value(&frozen.definition).unwrap();
    tampered["comparability_version"] = json!(99);
    std::fs::write(&path, serde_json::to_vec(&tampered).unwrap()).unwrap();
    let error =
        read_frozen_definition(&frozen.run_dir, &frozen.record.origin.definition).unwrap_err();
    assert!(refusal(&error).contains("no longer digests"), "{error:#}");
    let error = thaw(
        &launching.access,
        OWNER,
        "run-def",
        &launching.runs_dir(),
        Isolation::Embedded,
    )
    .await
    .unwrap_err();
    assert!(refusal(&error).contains("no longer digests"), "{error:#}");
}

#[tokio::test]
async fn a_run_frozen_without_the_file_falls_back_to_the_live_definition() {
    let launching = Launching::new().await;
    let pack = launching.pack("pack", "Off");
    let frozen = freeze(
        &launching.access,
        &launching.request("run-old", &pack),
        Isolation::Embedded,
    )
    .await
    .unwrap();
    std::fs::remove_file(frozen.run_dir.join(DEFINITION_FILE)).unwrap();
    assert_eq!(
        read_frozen_definition(&frozen.run_dir, &frozen.record.origin.definition).unwrap(),
        None
    );
    let thawed = thaw(
        &launching.access,
        OWNER,
        "run-old",
        &launching.runs_dir(),
        Isolation::Embedded,
    )
    .await
    .unwrap();
    assert_eq!(thawed.definition, frozen.definition);
}
