use super::*;
use crate::runner::scripted::ScriptedExecutor;
use crate::OutcomeKind;

const CAPTURE: &str = "workspace";
const CONFIG: &str = "[server]\nport = 8080\n";
const SEED: &str = "id,qty\n1,2\n";
const STATUS: &str = r#"{"state": "running", "pid": 4242, "ports": [8080]}"#;
const LOG: &str = "starting\nlistening on 8080\n";

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn file(path: &str, contents: &[u8]) -> FileRef {
    FileRef {
        path: path.into(),
        sha256: digest(contents),
        bytes: contents.len() as u64,
        text: std::str::from_utf8(contents).ok().map(str::to_owned),
    }
}

fn stage(files: Vec<FileRef>, outside: &[&str]) -> StageEvidence {
    let mut stage = ScriptedExecutor::passed_evidence("did:x", "s1", "rows", vec![])
        .stages
        .remove(0);
    stage.captures.insert(
        CAPTURE.into(),
        CaptureResult::Files {
            files,
            outside_workspace: outside.iter().map(|path| (*path).to_owned()).collect(),
        },
    );
    stage
}

fn finished() -> Vec<FileRef> {
    vec![
        file("bin/blob", &[0xff, 0x00]),
        file("config/app.toml", CONFIG.as_bytes()),
        file("logs/server.log", LOG.as_bytes()),
        file("run/status.json", STATUS.as_bytes()),
        file("seed/input.csv", SEED.as_bytes()),
    ]
}

fn judged(expect: Value, stage: &StageEvidence) -> CheckVerdict {
    CapturedFilesMatch.evaluate(&json!({"name": CAPTURE, "expect": expect}), stage)
}

fn feedback(verdict: &CheckVerdict) -> &str {
    verdict.feedback.as_deref().unwrap_or_default()
}

#[test]
fn every_kind_of_test_holds_over_the_files_the_stage_left() {
    let verdict = judged(
        json!([
            {"path": "config/app.toml", "equals": CONFIG},
            {"path": "seed/input.csv", "sha256": digest(SEED.as_bytes()).to_uppercase()},
            {"path": "logs/server.log", "contains": "listening on 8080"},
            {"path": "logs/server.log", "matches": r"(?m)^listening on \d+$"},
            {"path": "run/status.json", "pointer": "/state", "equals": "running"},
            {"path": "run/status.json", "pointer": "/ports/0", "equals": 8080},
            {"path": "run/status.json", "pointer": "/pid", "matches": "^42"},
            {"path": "run/status.json", "pointer": "", "equals": serde_json::from_str::<Value>(STATUS).unwrap()},
            {"path": "bin/blob", "exists": true},
            {"path": "run/server.lock", "exists": false}
        ]),
        &stage(finished(), &[]),
    );
    assert_eq!(
        (verdict.kind, verdict.score_bp, verdict.raw["total"].clone()),
        (OutcomeKind::Passed, Some(10_000), json!(10)),
        "{:?}",
        verdict.feedback
    );
    assert_eq!(verdict.feedback, None);
}

#[test]
fn each_unmet_expectation_costs_its_share_and_says_what_the_file_holds() {
    let files = vec![
        file("bin/blob", &[0xff, 0x00]),
        file("config/app.toml", b"[server]\nport = 8081\n"),
        file("logs/server.log", LOG.as_bytes()),
        file("run/server.lock", b"4242\n"),
        file("run/status.json", br#"{"state": "starting"}"#),
    ];
    let verdict = judged(
        json!([
            {"path": "config/app.toml", "equals": CONFIG},
            {"path": "seed/input.csv", "sha256": digest(SEED.as_bytes())},
            {"path": "seed/input.csv", "equals": SEED},
            {"path": "run/status.json", "pointer": "/state", "equals": "running"},
            {"path": "run/status.json", "pointer": "/pid", "equals": 4242},
            {"path": "run/server.lock", "exists": false},
            {"path": "logs/server.log", "contains": "listening"},
            {"path": "bin/blob", "exists": true}
        ]),
        &stage(files, &[]),
    );
    assert_eq!(
        (verdict.kind, verdict.score_bp),
        (OutcomeKind::ModelAcceptance, Some(2_500))
    );
    assert_eq!(
        feedback(&verdict).lines().collect::<Vec<_>>(),
        [
            r#"config/app.toml: expected text equal to "[server]\nport = 8080\n", got line 2 "port = 8081" where "port = 8080" was expected"#,
            &format!(
                "seed/input.csv: expected sha256 {}, got absent",
                digest(SEED.as_bytes())
            ),
            r#"seed/input.csv: expected text equal to "id,qty\n1,2\n", got absent"#,
            r#"run/status.json: expected JSON at "/state" equals "running", got "starting""#,
            r#"run/status.json: expected JSON at "/pid" equals 4242, got no value there"#,
            "run/server.lock: expected absent, got a file of 5 bytes",
        ]
    );
}

#[test]
fn a_text_difference_names_the_first_line_that_differs() {
    for (actual, got) in [
        (
            "[server]\n",
            r#"line 2 "" where "port = 8080" was expected"#,
        ),
        (
            "[server]\nport = 8080",
            r#"line 3 the end of the file where "" was expected"#,
        ),
        (
            "[server]\nport = 8080\nextra\n",
            r#"line 3 "extra" where "" was expected"#,
        ),
        (
            "[server]\r\nport = 8080\r\n",
            r#"line 1 "[server]\r" where "[server]" was expected"#,
        ),
    ] {
        let verdict = judged(
            json!([{"path": "config/app.toml", "equals": CONFIG}]),
            &stage(vec![file("config/app.toml", actual.as_bytes())], &[]),
        );
        assert!(feedback(&verdict).contains(got), "{actual:?}: {verdict:?}");
    }
}

#[test]
fn content_tests_of_bytes_that_are_not_text_or_not_json_fail_naming_what_they_are() {
    let verdict = judged(
        json!([
            {"path": "bin/blob", "contains": "x"},
            {"path": "bin/blob", "equals": "x"},
            {"path": "logs/server.log", "pointer": "/state", "equals": "running"},
            {"path": "seed/input.csv", "sha256": digest(b"id,qty\n")}
        ]),
        &stage(finished(), &[]),
    );
    assert_eq!(
        (verdict.kind, verdict.score_bp),
        (OutcomeKind::ModelAcceptance, Some(0))
    );
    let text = feedback(&verdict);
    assert!(
        text.contains(r#"bin/blob: expected contains "x", got bytes that are not UTF-8 text"#),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "bin/blob: expected text equal to \"x\", got sha256 {} (2 bytes)",
            digest(&[0xff, 0x00])
        )),
        "{text}"
    );
    assert!(
        text.contains("logs/server.log: expected JSON at \"/state\" equals \"running\", got text that is not JSON ("),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "got sha256 {} ({} bytes)",
            digest(SEED.as_bytes()),
            SEED.len()
        )),
        "{text}"
    );
}

#[test]
fn a_link_out_of_the_workspace_is_neither_present_nor_absent() {
    let verdict = judged(
        json!([
            {"path": "escape.txt", "exists": true},
            {"path": "escape.txt", "exists": false},
            {"path": "escape.txt", "contains": "secret"}
        ]),
        &stage(finished(), &["escape.txt"]),
    );
    assert_eq!(verdict.score_bp, Some(0));
    assert_eq!(
        feedback(&verdict)
            .lines()
            .filter(|line| line.ends_with(OUTSIDE))
            .count(),
        3,
        "{verdict:?}"
    );
}

#[test]
fn whole_file_digests_grade_a_file_too_large_to_keep_as_text() {
    let large = "x".repeat(FILE_TEXT_LIMIT_BYTES as usize + 1);
    let kept = FileRef {
        text: None,
        ..file("big.log", large.as_bytes())
    };
    let evidence = stage(vec![kept], &[]);
    let verdict = judged(
        json!([
            {"path": "big.log", "equals": large},
            {"path": "big.log", "sha256": digest(large.as_bytes())},
            {"path": "big.log", "exists": true}
        ]),
        &evidence,
    );
    assert_eq!(
        (verdict.kind, verdict.score_bp),
        (OutcomeKind::Passed, Some(10_000))
    );

    for expectation in [
        json!({"path": "big.log", "contains": "x"}),
        json!({"path": "big.log", "pointer": "/a", "equals": 1}),
    ] {
        let verdict = judged(json!([expectation]), &evidence);
        assert_eq!(
            (verdict.kind, verdict.score_bp, &verdict.raw["reason_code"]),
            (OutcomeKind::Grader, None, &json!("content_unavailable")),
            "{expectation}"
        );
        let detail = verdict.raw["detail"].as_str().unwrap_or_default();
        assert!(
            detail.contains("big.log") && detail.contains(&FILE_TEXT_LIMIT_BYTES.to_string()),
            "{detail}"
        );
    }
}

#[test]
fn a_missing_or_non_file_capture_is_a_grader_outcome() {
    let evidence = stage(finished(), &[]);
    for name in ["other", "rows"] {
        let verdict = CapturedFilesMatch.evaluate(
            &json!({"name": name, "expect": [{"path": "a.txt", "exists": false}]}),
            &evidence,
        );
        assert_eq!(
            (verdict.kind, verdict.score_bp, &verdict.raw["reason_code"]),
            (OutcomeKind::Grader, None, &json!("missing_capture")),
            "{name}"
        );
    }
}

#[test]
fn malformed_expectations_are_bad_params_that_name_the_expectation() {
    let hex = digest(b"");
    for (expect, problem) in [
        (json!([]), "expect names no file"),
        (json!([{"path": "a"}]), "give exactly one of"),
        (
            json!([{"path": "a", "exists": true, "contains": "x"}]),
            "give exactly one of",
        ),
        (
            json!([{"path": "a", "matches": "("}]),
            "matches is not a regex",
        ),
        (json!([{"path": "a", "sha256": "abc"}]), "not 64 hex digits"),
        (
            json!([{"path": "a", "pointer": "state", "equals": 1}]),
            "not a JSON pointer",
        ),
        (
            json!([{"path": "a", "pointer": "/~2", "equals": 1}]),
            "not a JSON pointer",
        ),
        (
            json!([{"path": "a", "pointer": "/x", "exists": true}]),
            "drop pointer",
        ),
        (
            json!([{"path": "a", "pointer": "/x", "sha256": hex}]),
            "drop pointer",
        ),
        (
            json!([{"path": "a", "equals": 1}]),
            "must be a string, not 1; add pointer",
        ),
        (
            json!([{"path": "../a", "exists": true}]),
            "relative to the trial workspace",
        ),
        (
            json!([{"path": "/etc/passwd", "exists": true}]),
            "relative to the trial workspace",
        ),
        (
            json!([{"path": "a//b", "exists": true}]),
            "relative to the trial workspace",
        ),
        (
            json!([{"path": "./a", "exists": true}]),
            "relative to the trial workspace",
        ),
        (
            json!([{"path": "a", "exists": true, "extra": 1}]),
            "unknown field",
        ),
    ] {
        let verdict = judged(expect.clone(), &stage(finished(), &[]));
        assert_eq!(
            (verdict.kind, verdict.score_bp, &verdict.raw["reason_code"]),
            (OutcomeKind::Grader, None, &json!("bad_params")),
            "{expect}"
        );
        let detail = verdict.raw["detail"].as_str().unwrap_or_default();
        assert!(detail.contains(problem), "{expect}: {detail}");
    }
    let verdict = judged(
        json!([{"path": "a", "exists": true}, {"path": "b", "matches": "["}]),
        &stage(finished(), &[]),
    );
    let detail = verdict.raw["detail"].as_str().unwrap_or_default();
    assert!(
        detail.starts_with("expect[1] \"b\": matches is not a regex"),
        "{detail}"
    );
}

#[test]
fn the_schema_holds_the_shape_of_each_expectation() {
    let validator =
        jsonschema::validator_for(&CapturedFilesMatch.describe().params_schema).unwrap();
    let params = |expect: Value| json!({"name": CAPTURE, "expect": expect});
    for valid in [
        json!([{"path": "a.txt", "exists": false}]),
        json!([{"path": "a.txt", "sha256": digest(b"")}]),
        json!([{"path": "a.json", "pointer": "", "equals": null}]),
        json!([{"path": "a.json", "pointer": "/a~1b/0", "contains": "x"}]),
        json!([{"path": "a.txt", "matches": "^x"}]),
    ] {
        assert!(validator.is_valid(&params(valid.clone())), "{valid}");
    }
    for invalid in [
        json!([]),
        json!([{"path": "a.txt"}]),
        json!([{"path": "", "exists": true}]),
        json!([{"path": "a.txt", "exists": true, "equals": "x"}]),
        json!([{"path": "a.txt", "sha256": "abc"}]),
        json!([{"path": "a.json", "pointer": "a", "equals": 1}]),
        json!([{"path": "a.json", "pointer": "/~2", "equals": 1}]),
        json!([{"path": "a.txt", "exists": "yes"}]),
        json!([{"path": "a.txt", "exists": true, "glob": "*"}]),
    ] {
        assert!(!validator.is_valid(&params(invalid.clone())), "{invalid}");
    }
    assert!(!validator.is_valid(&json!({"expect": [{"path": "a", "exists": true}]})));
}

fn declared(capture: Value) -> EvalStage {
    serde_json::from_value(json!({
        "stage_id": "edit",
        "prompt": "Change the port to 8080.",
        "capture": capture,
    }))
    .unwrap()
}

#[test]
fn conformance_reads_the_params_against_the_captures_their_stage_declares() {
    let stage = declared(json!([
        {"kind": "file", "name": CAPTURE, "glob": "config/*.toml"},
        {"kind": "file", "name": "everything", "glob": "config/**"},
        {"kind": "file", "name": "broken", "glob": "config/[.toml"},
        {"kind": "documents", "name": "rows", "collection": "CanaryItem", "filter": {}}
    ]));
    let problems = |name: &str, expect: Value| {
        CapturedFilesMatch.conformance(&json!({"name": name, "expect": expect}), &stage)
    };
    assert_eq!(
        problems(
            CAPTURE,
            json!([
                {"path": "config/app.toml", "equals": CONFIG},
                {"path": "config/.local.toml", "exists": false}
            ])
        ),
        Vec::<String>::new()
    );
    assert_eq!(
        problems(
            CAPTURE,
            json!([
                {"path": "config/app.toml", "exists": true},
                {"path": "config/old/app.toml", "exists": false},
                {"path": "seed/input.csv", "equals": SEED}
            ])
        ),
        [
            "expect[1] \"config/old/app.toml\" is outside capture workspace's glob \"config/*.toml\", so the capture never holds it",
            "expect[2] \"seed/input.csv\" is outside capture workspace's glob \"config/*.toml\", so the capture never holds it",
        ]
    );
    let only = |name: &str| {
        let found = problems(name, json!([{"path": "config/app.toml", "exists": false}]));
        assert_eq!(found.len(), 1, "{name}: {found:?}");
        found.into_iter().next().unwrap()
    };
    assert!(
        only("missing").contains("declares no capture \"missing\"; add {\"kind\": \"file\""),
        "{}",
        only("missing")
    );
    assert!(only("rows").contains("does not capture files"));
    assert!(only("everything").contains("ends in **"));
    assert!(only("broken").contains("does not compile"));
    assert_eq!(
        problems(
            CAPTURE,
            json!([{"path": "config/app.toml", "matches": "("}])
        )
        .len(),
        1,
        "malformed params report their own problems first"
    );
}
