use super::*;
use crate::runner::embedded::executor::install::{apply, desired};
use crate::runner::embedded::home::EmbeddedHome;
use crate::OutcomeKind;
use gents_documents::DesiredStateApplyPlan;
use gents_store::{collection::Collection, config_client::ConfigAccess};

use serde_json::json;

#[tokio::test]
async fn schema_capture_reads_active_indexes_and_records_absent_collections() {
    let node_db = gents_inference::test_support::test_db(
        "schema-capture-reads-active-indexes-and-",
        gents_automation::lifecycle_hooks::composition(),
    )
    .await;
    let node = node_db.node.clone();
    ConfigAccess::local(node.clone())
        .add_collection("type SearchNote { body: String @fulltext(language: \"english\") }")
        .await
        .unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let captures = run_captures(
        &node,
        "did:test",
        "session-test",
        workspace.path(),
        &[
            Capture::Schema {
                name: "present".into(),
                collections: vec!["SearchNote".into()],
            },
            Capture::Schema {
                name: "absent".into(),
                collections: vec!["NoSuchCollection".into()],
            },
        ],
        CaptureBindings::default(),
    )
    .await;
    let CaptureResult::Schema { collections } = &captures["present"] else {
        panic!("schema evidence")
    };
    assert_eq!(collections[0]["FullTextIndexes"][0]["FieldName"], "body");
    assert_eq!(
        captures["absent"],
        CaptureResult::Schema {
            collections: vec![]
        }
    );
}

#[tokio::test]
async fn delegation_case_capture_excludes_session_title_requests() {
    let home = EmbeddedHome::create_temp("eval-normal-request-capture")
        .await
        .unwrap();
    let did = escape_graphql_string(home.did());
    for purpose in ["normal", "title-audit"] {
        let purpose = escape_graphql_string(purpose);
        ConfigAccess::write_local(
            &home.node,
            "eval.test.normal_request_capture",
            &format!(r#"mutation {{ create_AgentRequest(input: {{request_id: "{purpose}", purpose: "{purpose}", node_did: "{did}", agent_did: "{did}:research-helper", content: "TRAIN-111"}}) {{ _docID }} }}"#),
        )
        .await
        .unwrap();
    }
    let case: Value = serde_json::from_slice(
        &std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../gents").join(
            "tests/fixtures/configurator_evals/ladder/l5_agents_tools/cases/train_minimal_self_target.json",
        ))
        .unwrap(),
    )
    .unwrap();
    let capture = case["stages"][1]["capture"]
        .as_array()
        .unwrap()
        .iter()
        .find(|capture| capture["name"] == "helper-request")
        .unwrap();
    let fields: Vec<String> = serde_json::from_value(capture["fields"].clone()).unwrap();
    let role = BTreeMap::from([(
        "role_agents".into(),
        CaptureResult::Documents {
            rows: vec![
                json!({"display_name":"Research Helper", "agent_did":format!("{did}:research-helper")}),
            ],
        },
    )]);
    let bound = bind_capture_subject(
        &capture["filter"],
        "unused",
        CaptureBindings::default(),
        &role,
    )
    .unwrap();
    let mut all_purposes = bound.clone();
    all_purposes.as_object_mut().unwrap().remove("purpose");
    let all = capture_documents(
        &home.node,
        "AgentRequest",
        &all_purposes,
        &fields,
        home.did(),
    )
    .await
    .unwrap();
    assert_eq!(all.len(), 2);
    let normal = capture_documents(&home.node, "AgentRequest", &bound, &fields, home.did())
        .await
        .unwrap();
    assert_eq!(normal.len(), 1);
    assert_eq!(normal[0]["request_id"], "normal");
    home.node.shutdown().await.expect("shutdown node");
}

#[tokio::test]
async fn document_capture_excludes_other_sessions_with_the_same_sequence() {
    let node_db = gents_inference::test_support::test_db(
        "document-capture-excludes-other-sessions",
        gents_automation::lifecycle_hooks::composition(),
    )
    .await;
    let node = node_db.node.clone();
    let access = ConfigAccess::local(node.clone());
    access
        .add_collection("type CaptureHeader { session_id: String sequence: Int }")
        .await
        .unwrap();
    let session_id = "trial\"session";
    for session in [session_id, "background-session"] {
        access.write("eval.test.capture_session", &format!(
            "mutation {{ create_CaptureHeader(input: {{session_id: \"{}\", sequence: 1}}) {{ _docID }} }}",
            escape_graphql_string(session)
        )).await.unwrap();
    }
    let workspace = tempfile::tempdir().unwrap();
    let captures = run_captures(
        &node,
        "did:test",
        session_id,
        workspace.path(),
        &[Capture::Documents {
            name: "source_headers".into(),
            collection: "CaptureHeader".into(),
            filter: json!({"session_id": {"_eq": "$session"}, "sequence": {"_eq": 1}}),
            fields: vec!["session_id".into(), "sequence".into()],
        }],
        CaptureBindings::default(),
    )
    .await;
    let CaptureResult::Documents { rows } = &captures["source_headers"] else {
        panic!("document capture")
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["session_id"], session_id);
    assert_eq!(rows[0]["sequence"], 1);
    assert!(rows[0]["_docID"].as_str().is_some());
    assert_eq!(
        bind_capture_session(&json!("literal $session text"), session_id),
        json!("literal $session text")
    );
    node.shutdown().await.expect("shutdown node");
}

#[test]
fn capture_query_renders_filter_and_escapes_strings() {
    let query = documents_capture_query(
        "AgentRequest",
        &json!({"requester_did": {"_eq": "$trial"}}),
        &["content".to_string()],
        r#"did:x"y"#,
    )
    .unwrap();
    assert!(
        query.contains(r#"filter: {requester_did: {_eq: "did:x\"y"}}"#),
        "{query}"
    );
    assert!(
        query.contains("{ _docID content requester_did }"),
        "{query}"
    );
}

#[test]
fn document_captures_reject_secret_fields_and_oauth_documents() {
    let error = documents_capture_query(
        "InferenceBackend",
        &json!({"node_did": {"_eq": "did:test"}}),
        &["backend_id".into(), "auth".into()],
        "did:test",
    )
    .unwrap_err();
    assert!(error.to_string().contains("InferenceBackend"));
    assert!(error.to_string().contains("restricted"));

    let error = documents_capture_query(
        "InferenceBackend",
        &json!({"_or": [{"auth": {"_eq": {"kind": "api_key"}}}]}),
        &["backend_id".into()],
        "did:test",
    )
    .unwrap_err();
    assert!(error.to_string().contains("restricted"));

    let error = documents_capture_query(
        "OAuthCredential",
        &json!({}),
        &["credential_id".into()],
        "did:test",
    )
    .unwrap_err();
    assert!(error.to_string().contains("credential collection"));

    let safe = documents_capture_query(
        "Task",
        &json!({"node_did": {"_eq": "did:test"}}),
        &["task_id".into(), "display_name".into()],
        "did:test",
    )
    .unwrap();
    assert!(safe.contains("task_id display_name"));
}

#[test]
fn generated_document_capture_cases_match_lean() {
    use std::collections::BTreeSet;

    gents_lean_vocab::families::eval_document_capture::replay(|case| {
        let filter: Value = serde_json::from_str(&case.filter).unwrap();
        let mut filter_fields = Vec::new();
        gents_tools::builtin::defra_query::collect_filter_field_keys(&filter, &mut filter_fields);
        assert_eq!(
            filter_fields.into_iter().collect::<BTreeSet<_>>(),
            case.filter_fields.iter().cloned().collect::<BTreeSet<_>>(),
            "{case:?}"
        );
        documents_capture_query(&case.collection, &filter, &case.fields, "did:test").is_ok()
    });
}

#[tokio::test]
async fn l1_backend_capture_grades_metadata_without_retaining_auth() {
    let mut home = EmbeddedHome::create_temp("eval-backend-metadata")
        .await
        .unwrap();
    let evaluator = crate::runner::embedded::home::bind_test_evaluator(&mut home);
    let evaluator_did = gents_store::identity::NodeIdentity::did(evaluator.as_ref());
    crate::runner::embedded::executor::install::grant_trial_operations(&home, evaluator_did)
        .await
        .unwrap();
    let access = home.client_access(evaluator_did).unwrap();
    crate::test_support::bootstrap_signed_test_node(&access, home.did()).await;
    let plan = DesiredStateApplyPlan::new(vec![desired(
        Collection::InferenceBackend,
        json!({
            "node_did": home.did(), "backend_id": "metadata-backend", "name": "metadata",
            "provider_kind": "OpenAiCompatible", "endpoint": "http://127.0.0.1:9/v1",
            "auth": {"kind": "api_key", "key": "eval-secret-sentinel"}, "enabled": true
        }),
    )])
    .unwrap();
    apply(&access, "eval.test.backend_metadata", &plan)
        .await
        .unwrap();

    let case: Value = serde_json::from_slice(
        &std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../gents").join(
            "tests/fixtures/configurator_evals/ladder/l1_inference/cases/val_minimal_limits.json",
        ))
        .unwrap(),
    )
    .unwrap();
    let stage = &case["stages"][1];
    let capture: Capture = serde_json::from_value(
        stage["capture"]
            .as_array()
            .unwrap()
            .iter()
            .find(|capture| capture["name"] == "backends")
            .unwrap()
            .clone(),
    )
    .unwrap();
    let mut evidence = crate::runner::ScriptedExecutor::passed_evidence(
        home.did(),
        "configure",
        "session-metadata",
        Vec::new(),
    )
    .stages
    .remove(0);
    evidence.captures = run_captures_with_access(
        &access,
        home.did(),
        "session-metadata",
        home.path(),
        &[capture],
        CaptureBindings {
            evaluator_did: Some(evaluator_did),
            ..CaptureBindings::default()
        },
    )
    .await;
    let CaptureResult::Documents { rows } = &evidence.captures["backends"] else {
        panic!("backend metadata capture must contain documents");
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["backend_id"], "metadata-backend");
    assert!(rows[0].get("auth").is_none());
    assert!(!serde_json::to_string(&evidence.captures)
        .unwrap()
        .contains("eval-secret-sentinel"));

    let check = stage["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["check"] == "captured_rows_count")
        .unwrap();
    let registry = crate::checks::CheckRegistry::builtin();
    let verdict = registry
        .get("captured_rows_count")
        .unwrap()
        .evaluate(&check["params"], &evidence);
    assert_eq!(verdict.kind, OutcomeKind::Passed, "{:?}", verdict.raw);
    home.node.shutdown().await.expect("shutdown node");
}

fn capture(
    workspace: &Path,
    glob: &str,
    text_paths: &[&str],
) -> (Vec<FileRef>, Vec<String>, Vec<String>) {
    let text_paths = text_paths.iter().map(|path| (*path).to_owned()).collect();
    match capture_files(workspace, glob, &text_paths) {
        Some(CaptureResult::Files {
            files,
            outside_workspace,
            non_files,
        }) => (files, outside_workspace, non_files),
        other => panic!("expected a file capture, got {other:?}"),
    }
}

fn captured(workspace: &Path, glob: &str) -> (Vec<FileRef>, Vec<String>) {
    let (files, outside_workspace, _) = capture(workspace, glob, &[]);
    (files, outside_workspace)
}

#[test]
fn file_capture_refuses_parent_components() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("report.txt"), b"hello").unwrap();
    std::fs::write(dir.path().join("outside.txt"), b"secret").unwrap();

    assert_eq!(
        capture_files(&workspace, "../*.txt", &BTreeSet::new()),
        None
    );
    assert_eq!(capture_files(&workspace, "/etc/*", &BTreeSet::new()), None);
    let (found, outside) = captured(&workspace, "*.txt");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].path, "report.txt");
    assert_eq!(found[0].bytes, 5);
    assert!(outside.is_empty());
}

#[test]
fn file_capture_keeps_text_only_for_named_utf8_files_within_the_limit() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let limit = FILE_TEXT_LIMIT_BYTES as usize;
    std::fs::write(workspace.join("a-small.txt"), "héllo\n").unwrap();
    std::fs::write(workspace.join("b-binary.txt"), [0xff, 0xfe, 0x00]).unwrap();
    std::fs::write(workspace.join("c-at-limit.txt"), "x".repeat(limit)).unwrap();
    std::fs::write(workspace.join("d-over-limit.txt"), "x".repeat(limit + 1)).unwrap();
    std::fs::write(workspace.join("e-unnamed.txt"), "hello\n").unwrap();

    let (files, _, _) = capture(
        workspace,
        "*.txt",
        &[
            "a-small.txt",
            "b-binary.txt",
            "c-at-limit.txt",
            "d-over-limit.txt",
        ],
    );
    let kept: Vec<(&str, u64, Option<FileText>)> = files
        .iter()
        .map(|file| (file.path.as_str(), file.bytes, file.text.clone()))
        .collect();
    assert_eq!(
        kept,
        [
            ("a-small.txt", 7, Some(FileText::Kept("héllo\n".into()))),
            ("b-binary.txt", 3, Some(FileText::NotUtf8)),
            (
                "c-at-limit.txt",
                limit as u64,
                Some(FileText::Kept("x".repeat(limit)))
            ),
            (
                "d-over-limit.txt",
                limit as u64 + 1,
                Some(FileText::OverLimit)
            ),
            ("e-unnamed.txt", 6, None),
        ]
    );
    assert_eq!(
        files[3].sha256,
        format!("{:x}", Sha256::digest("x".repeat(limit + 1))),
        "a file whose text is not kept is still graded by its digest"
    );
}

#[test]
fn a_broad_file_capture_keeps_no_more_text_than_its_named_paths() {
    let dir = tempfile::tempdir().unwrap();
    let page = "x".repeat(4 * 1024);
    let tree: Vec<(String, &str)> = (0..64)
        .map(|index| (format!("src/m{index:02}/lib.rs"), page.as_str()))
        .collect();
    write_tree(
        dir.path(),
        &tree
            .iter()
            .map(|(path, contents)| (path.as_str(), *contents))
            .collect::<Vec<_>>(),
    );

    let (files, _, _) = capture(dir.path(), "**/*", &["src/m07/lib.rs", "src/m63/lib.rs"]);
    assert_eq!(files.len(), 64);
    let kept: Vec<(&str, usize)> = files
        .iter()
        .filter_map(|file| match &file.text {
            Some(FileText::Kept(text)) => Some((file.path.as_str(), text.len())),
            _ => None,
        })
        .collect();
    assert_eq!(
        kept,
        [
            ("src/m07/lib.rs", page.len()),
            ("src/m63/lib.rs", page.len())
        ],
        "kept text is bounded by the named paths, not by the glob"
    );
    assert!(files
        .iter()
        .filter(|file| !kept.iter().any(|(path, _)| *path == file.path))
        .all(|file| file.text.is_none()));
}

#[cfg(unix)]
#[test]
fn a_directory_or_dangling_link_at_a_path_is_recorded_and_is_not_absent() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    std::fs::create_dir_all(workspace.join("out/report.txt")).unwrap();
    std::fs::create_dir_all(workspace.join("run")).unwrap();
    std::os::unix::fs::symlink(
        workspace.join("run/no-such-target"),
        workspace.join("run/server.lock"),
    )
    .unwrap();
    std::fs::write(workspace.join("notes.txt"), "n").unwrap();

    let (files, outside, non_files) = capture(workspace, "**/*", &[]);
    assert_eq!(
        files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        ["notes.txt"]
    );
    assert!(outside.is_empty());
    assert_eq!(
        non_files,
        ["out", "out/report.txt", "run", "run/server.lock"]
    );
    let (_, _, literal) = capture(workspace, "run/server.lock", &[]);
    assert_eq!(
        literal,
        ["run/server.lock"],
        "a literal glob reaches a dangling link too"
    );

    let mut stage = crate::runner::ScriptedExecutor::passed_evidence("did:x", "s", "rows", vec![])
        .stages
        .remove(0);
    stage.captures.insert(
        "workspace".into(),
        capture_files(workspace, "**/*", &BTreeSet::new()).unwrap(),
    );
    let verdict = crate::checks::CheckRegistry::builtin()
        .evaluate_bound(
            "captured_files_match",
            &json!({"name": "workspace", "expect": [
                {"path": "out/report.txt", "exists": false},
                {"path": "run/server.lock", "exists": false},
                {"path": "out/report.txt", "exists": true},
                {"path": "gone.txt", "exists": false}
            ]}),
            &stage,
        )
        .unwrap();
    assert_eq!(
        (verdict.kind, verdict.score_bp),
        (OutcomeKind::ModelAcceptance, Some(2_500)),
        "{:?}",
        verdict.feedback
    );
    let feedback = verdict.feedback.unwrap_or_default();
    for path in ["out/report.txt", "run/server.lock"] {
        assert!(
            feedback.contains(&format!(
                "{path}: expected absent, got an entry that is not a regular file"
            )),
            "{feedback}"
        );
    }
}

fn write_tree(workspace: &Path, files: &[(&str, &str)]) {
    for (path, contents) in files {
        let path = workspace.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
}

#[test]
fn captured_files_match_names_paths_outside_a_glob_exactly_as_the_capture_walk_skips_them() {
    use crate::checks::captured_files_match::CapturedFilesMatch;
    use crate::checks::Check;

    let dir = tempfile::tempdir().unwrap();
    let paths = [
        "a.txt",
        ".hidden.txt",
        ".cache/e.txt",
        "dir/b.txt",
        "dir/sub/c.json",
        "dir/sub/d.txt",
        "out/x/y.md",
    ];
    write_tree(dir.path(), &paths.map(|path| (path, "x")));
    let params = json!({"name": "files", "expect": paths
        .iter()
        .map(|path| json!({"path": path, "exists": true}))
        .collect::<Vec<_>>()});
    for glob in [
        "*.txt",
        "?.txt",
        "**/*.txt",
        "**/*",
        "*/*.txt",
        "[ad]*/*.txt",
        "dir/*",
        "dir/**/*.json",
        "dir/sub/c.json",
        "out/**",
    ] {
        let (files, _) = captured(dir.path(), glob);
        let held: BTreeSet<&str> = files.iter().map(|file| file.path.as_str()).collect();
        let stage: gents_documents::EvalStage = serde_json::from_value(json!({
            "stage_id": "s",
            "prompt": "p",
            "capture": [{"kind": "file", "name": "files", "glob": glob}],
        }))
        .unwrap();
        let problems = CapturedFilesMatch.conformance(&params, &stage);
        let reachable: BTreeSet<&str> = if problems.iter().any(|p| p.contains("ends in **")) {
            BTreeSet::new()
        } else {
            paths
                .into_iter()
                .filter(|path| {
                    !problems
                        .iter()
                        .any(|problem| problem.contains(&format!("{path:?} is outside")))
                })
                .collect()
        };
        assert_eq!(reachable, held, "{glob}: {problems:?}");
    }
}

#[test]
fn a_file_capture_grades_host_file_content_and_untouched_seeds() {
    let dir = tempfile::tempdir().unwrap();
    let seed = "id,qty\n1,2\n";
    write_tree(
        dir.path(),
        &[
            ("config/app.toml", "[server]\nport = 8080\n"),
            ("run/status.json", r#"{"state": "running", "pid": 4242}"#),
            ("seed/input.csv", seed),
            ("notes/unread.md", "not graded"),
        ],
    );
    let case: gents_documents::EvalCase = serde_json::from_value(json!({
        "case_id": "edit",
        "split": "train",
        "stages": [{
            "stage_id": "edit",
            "prompt": "Serve on 8080 and leave the seed alone.",
            "capture": [{"kind": "file", "name": "workspace", "glob": "**/*"}],
            "checks": [{
                "check": "captured_files_match",
                "tier": "acceptance",
                "params": {"name": "workspace", "expect": [
                    {"path": "config/app.toml", "contains": "port = 8080"},
                    {"path": "run/status.json", "pointer": "/state", "equals": "running"},
                    {"path": "seed/input.csv", "equals": seed},
                    {"path": "seed/input.csv.bak", "exists": false}
                ]}
            }]
        }]
    }))
    .unwrap();
    let specs = crate::runner::spec::stage_specs(&case, &[]);
    let check = &case.stages[0].checks[0];
    let registry = crate::checks::CheckRegistry::builtin();
    let grade = || {
        let mut stage =
            crate::runner::ScriptedExecutor::passed_evidence("did:x", "s", "rows", vec![])
                .stages
                .remove(0);
        stage.captures = specs[0]
            .captures
            .iter()
            .filter_map(|capture| match capture {
                Capture::File {
                    name,
                    glob,
                    text_paths,
                } => {
                    capture_files(dir.path(), glob, text_paths).map(|result| (name.clone(), result))
                }
                _ => None,
            })
            .collect();
        let CaptureResult::Files { files, .. } = &stage.captures["workspace"] else {
            panic!("a file capture");
        };
        assert_eq!(
            files
                .iter()
                .filter(|file| file.text.is_some())
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            ["config/app.toml", "run/status.json", "seed/input.csv"],
            "text is kept for the paths the check reads, and no other"
        );
        registry
            .evaluate_bound(&check.check, &check.params, &stage)
            .unwrap()
    };
    let verdict = grade();
    assert_eq!(
        (verdict.kind, verdict.score_bp),
        (OutcomeKind::Passed, Some(10_000)),
        "{:?}",
        verdict.feedback
    );

    std::fs::write(dir.path().join("seed/input.csv"), "id,qty\n1,3\n").unwrap();
    std::fs::write(dir.path().join("seed/input.csv.bak"), seed).unwrap();
    let verdict = grade();
    assert_eq!(
        (verdict.kind, verdict.score_bp),
        (OutcomeKind::ModelAcceptance, Some(5_000))
    );
    let feedback = verdict.feedback.unwrap_or_default();
    assert!(
        feedback.contains(r#"seed/input.csv: expected text equal to "id,qty\n1,2\n", got line 2 "1,3" where "1,2" was expected"#),
        "{feedback}"
    );
    assert!(
        feedback.contains("seed/input.csv.bak: expected absent, got a file of 11 bytes"),
        "{feedback}"
    );
}

#[cfg(unix)]
#[test]
fn file_capture_does_not_follow_a_symlink_out_of_the_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("mine.txt"), b"mine").unwrap();
    let secret = dir.path().join("secret.txt");
    std::fs::write(&secret, b"not the trial's").unwrap();
    std::os::unix::fs::symlink(&secret, workspace.join("escape.txt")).unwrap();

    let (files, outside) = captured(&workspace, "*.txt");
    assert_eq!(
        files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        ["mine.txt"],
        "only the file the trial itself holds is captured"
    );
    assert_eq!(
        outside,
        ["escape.txt"],
        "the skipped match is recorded, not silently dropped"
    );
}

#[cfg(unix)]
#[test]
fn a_file_capture_that_cannot_be_read_is_omitted() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let locked = workspace.join("locked.txt");
    std::fs::write(&locked, b"unreadable").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&locked).is_ok() {
        return;
    }

    assert_eq!(
        capture_files(&workspace, "*.txt", &BTreeSet::new()),
        None,
        "a capture that cannot be read is absent, not empty"
    );

    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o600)).unwrap();
    let (files, outside) = captured(&workspace, "*.txt");
    assert_eq!(
        files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        ["locked.txt"]
    );
    assert!(outside.is_empty());
}

#[test]
fn capture_filters_bind_only_the_selected_subject() {
    use crate::runner::subject_binding::SubjectAgent;
    let subject = SubjectAgent {
        slot: "engineer".into(),
        agent_did: "did:key:actual-subject".into(),
    };
    let bindings = CaptureBindings {
        subject: Some(&subject),
        evaluator_did: Some("did:key:evaluator"),
        request_id: None,
        ..Default::default()
    };
    let filter = json!({"agent_did":{"_ne":"gents:agent-slot:engineer"}, "session_id":{"_eq":"$session"}, "requester_did":{"_eq":"$evaluator"}});
    assert_eq!(
        bind_capture_subject(&filter, "session", bindings, &BTreeMap::new()).unwrap(),
        json!({"agent_did":{"_ne":"did:key:actual-subject"}, "session_id":{"_eq":"session"}, "requester_did":{"_eq":"did:key:evaluator"}})
    );
    assert!(bind_capture_subject(
        &filter,
        "session",
        CaptureBindings::default(),
        &BTreeMap::new()
    )
    .is_err());
    assert!(bind_capture_subject(
        &filter,
        "session",
        CaptureBindings {
            subject: Some(&subject),
            evaluator_did: None,
            request_id: None,
            ..Default::default()
        },
        &BTreeMap::new()
    )
    .is_err());
    assert!(bind_capture_subject(
        &json!({"agent_did":{"_eq":"gents:agent-slot:foreign#engineer"}}),
        "session",
        bindings,
        &BTreeMap::new()
    )
    .is_err());
    assert_eq!(
        bind_capture_subject(
            &json!({"requester_did":{"_eq":"literal $evaluator"}}),
            "session",
            bindings,
            &BTreeMap::new()
        )
        .unwrap(),
        json!({"requester_did":{"_eq":"literal $evaluator"}})
    );
}

#[tokio::test]
async fn retained_capture_uses_the_evaluator_actor_for_document_access() {
    use gents_store::config_client::NodePermission;
    use gents_store::identity::{KeyIdentity, NodeIdentity};

    let home = EmbeddedHome::create_temp("capture-evaluator-acp")
        .await
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let reader = KeyIdentity::load_or_create(directory.path().join("reader.key"), None).unwrap();
    let owner =
        ConfigAccess::local_as(home.node.clone(), ::identity::Did::new(home.did()).unwrap());
    owner.write("eval.test.capture_actor", &format!(
        "mutation {{create_AgentRequest(input: {{request_id: \"owned\", node_did: \"{}\", agent_did: \"owned-agent\", purpose: \"normal\"}}) {{_docID}}}}",
        escape_graphql_string(home.did()),
    )).await.unwrap();
    let reader_did = ::identity::Did::new(reader.did()).unwrap();
    for permission in [NodePermission::CollectionGet, NodePermission::DocumentRead] {
        owner
            .grant_node_permission(&reader_did, permission)
            .await
            .unwrap();
    }
    let captures = [Capture::Documents {
        name: "requests".into(),
        collection: "AgentRequest".into(),
        filter: json!({"request_id":{"_eq":"owned"}}),
        fields: vec!["request_id".into()],
    }];
    let owner_rows = run_captures(
        &home.node,
        home.did(),
        "s",
        directory.path(),
        &captures,
        CaptureBindings {
            subject: None,
            evaluator_did: Some(home.did()),
            request_id: None,
            ..Default::default()
        },
    )
    .await;
    let CaptureResult::Documents { rows } = &owner_rows["requests"] else {
        panic!("owner capture")
    };
    assert_eq!(rows.len(), 1);
    let reader_rows = run_captures(
        &home.node,
        home.did(),
        "s",
        directory.path(),
        &captures,
        CaptureBindings {
            subject: None,
            evaluator_did: Some(reader.did()),
            request_id: None,
            ..Default::default()
        },
    )
    .await;
    let CaptureResult::Documents { rows } = &reader_rows["requests"] else {
        panic!("reader capture")
    };
    assert!(rows.is_empty());
    home.node.shutdown().await.unwrap();
}

#[test]
fn capture_reference_requires_one_named_agent_and_its_actual_did() {
    let filter = json!({"agent_did":{"_eq":{"$capture":{"capture":"agents", "source_match":{"field":"display_name","matches":"(?i)^research[ -]helper$"},"field":"agent_did"}}}});
    let bind = |rows| {
        bind_capture_subject(
            &filter,
            "s",
            CaptureBindings::default(),
            &BTreeMap::from([("agents".into(), CaptureResult::Documents { rows })]),
        )
    };
    let role = json!({"display_name":"Research Helper","agent_did":"did:key:random"});
    let decoy = json!({"display_name":"Wrong Helper","agent_did":"did:key:research-helper"});
    assert_eq!(
        bind(vec![role.clone(), decoy.clone()]).unwrap(),
        json!({"agent_did":{"_eq":"did:key:random"}})
    );
    assert!(bind(vec![decoy]).is_err());
    assert!(bind(vec![role.clone(), role]).is_err());
    assert!(bind(vec![json!({"display_name":"Research Helper"})]).is_err());
}

#[test]
fn nightly_reviewer_fixture_captures_requests_for_the_actual_unique_agent() {
    let case: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../gents/tests/fixtures/configurator_evals/ladder/l1_inference/cases/held_nightly_automation_reviewer.json"
    )))
    .unwrap();
    let fire = case["stages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|stage| stage["stage_id"] == "fire")
        .unwrap();
    let captures = fire["capture"].as_array().unwrap();
    assert_eq!(captures[0]["name"], "role_agents");
    let request = captures
        .iter()
        .find(|capture| capture["name"] == "completed_review")
        .unwrap();
    let bind = |rows| {
        bind_capture_subject(
            &request["filter"],
            "unused",
            CaptureBindings::default(),
            &BTreeMap::from([("role_agents".into(), CaptureResult::Documents { rows })]),
        )
    };
    let reviewer =
        json!({"display_name":"Nightly Reviewer", "agent_did":"did:key:actual-reviewer"});
    let decoy = json!({"display_name":"Other Reviewer", "agent_did":"did:key:nightly-reviewer"});
    let bound = bind(vec![reviewer.clone(), decoy.clone()]).unwrap();
    assert_eq!(bound["agent_did"], json!({"_eq":"did:key:actual-reviewer"}));
    assert_eq!(bound["content"], json!({"_like":"%nightly-digest%"}));
    assert_eq!(bound["lifecycle_state"], json!({"_eq":"completed"}));
    assert!(bind(vec![decoy]).is_err());
    assert!(bind(vec![reviewer.clone(), reviewer]).is_err());
}

#[test]
fn capture_request_binding_requires_the_current_stage_request() {
    let filter = json!({"request_id":{"_eq":"$request"}});
    let bindings = CaptureBindings {
        request_id: Some("current"),
        ..Default::default()
    };
    assert_eq!(
        bind_capture_subject(&filter, "s", bindings, &BTreeMap::new()).unwrap(),
        json!({"request_id":{"_eq":"current"}})
    );
    assert!(
        bind_capture_subject(&filter, "s", CaptureBindings::default(), &BTreeMap::new()).is_err()
    );
}

#[tokio::test]
async fn logical_backend_capture_and_live_counts_use_held_user_metadata() {
    use crate::runner::embedded::executor::install::install;
    use crate::runner::embedded::executor::tests::{
        frozen_binding, materialized_digest, write_slot_pack,
    };
    use crate::runner::embedded::live::LiveObserver;
    use crate::runner::executor::{StageSpec, TrialSpec};

    let dir = tempfile::tempdir().unwrap();
    let pack = dir.path().join("pack");
    write_slot_pack(&pack, "gents:inference-slot:primary");
    let mut home = EmbeddedHome::create_temp("backend-capture").await.unwrap();
    let evaluator = Arc::new(
        gents_store::identity::KeyIdentity::load_or_create(dir.path().join("evaluator.key"), None)
            .unwrap(),
    );
    let evaluator_did = gents_store::identity::NodeIdentity::did(evaluator.as_ref()).to_owned();
    let capability = evaluator.client_signing_capability().unwrap();
    home.set_evaluator(evaluator, capability);
    let spec = TrialSpec {
        pack_digest: materialized_digest(&pack),
        pack_dir: pack,
        inference: frozen_binding(json!("frozen-profile")),
        agent_slot: "subject".into(),
        evaluator_did: evaluator_did.clone(),
        ..TrialSpec::empty_for_tests("backend-capture")
    };
    install(&spec, &home, &dir.path().join("workspace"), &[])
        .await
        .unwrap();
    let access = home.client_access(&evaluator_did).unwrap();
    let fields = vec!["backend_id".into(), "endpoint".into(), "enabled".into()];
    let filter = json!({"_and": [
        {"node_did": {"_eq": "$trial"}},
        {"backend_id": {"_eq": "frozen-backend"}}
    ]});
    let rows =
        capture_documents_with_access(&access, "InferenceBackend", &filter, &fields, home.did())
            .await
            .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["backend_id"], "frozen-backend");
    assert_eq!(rows[0]["endpoint"], "http://127.0.0.1:1/v1");
    assert_eq!(rows[0]["enabled"], true);
    assert!(rows[0]["_docID"].as_str().is_some_and(|id| !id.is_empty()));
    assert_eq!(rows[0].as_object().unwrap().len(), 4);
    let absent = json!({"node_did": {"_eq": "$trial"}, "backend_id": {"_eq": "other"}});
    assert!(capture_documents_with_access(
        &access,
        "InferenceBackend",
        &absent,
        &fields,
        home.did()
    )
    .await
    .unwrap()
    .is_empty());
    for (filter, fields) in [
        (json!({}), vec!["auth".to_owned()]),
        (json!({"auth": {"_eq": "secret"}}), Vec::new()),
        (json!({"credential_id": {"_eq": "secret"}}), Vec::new()),
        (json!({}), vec!["credential_id".to_owned()]),
    ] {
        assert!(capture_documents_with_access(
            &access,
            "InferenceBackend",
            &filter,
            &fields,
            home.did()
        )
        .await
        .is_err());
    }
    let stages = vec![StageSpec {
        stage_id: "check".into(),
        prompt: String::new(),
        seed: None,
        expect_fire: true,
        deadline_secs: None,
        settle: false,
        continuation: None,
        review_previous: false,
        captures: vec![Capture::Documents {
            name: "backends".into(),
            collection: "InferenceBackend".into(),
            filter,
            fields,
        }],
    }];
    let live = LiveObserver::new(&home.node, &access, home.did(), &stages)
        .await
        .snapshot()
        .await
        .unwrap();
    assert_eq!(live.documents.get("InferenceBackend"), Some(&1));
    assert_eq!(live.captures.get("backends"), Some(&1));
    home.node.shutdown().await.unwrap();
}
