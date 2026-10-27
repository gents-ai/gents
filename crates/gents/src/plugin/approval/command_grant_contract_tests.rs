use super::*;
use gents_loop::tool_policy::workspace_permissions as policy;

fn key(root: &Path, id: u64) -> workspace::WorkspaceKey {
    let path = root.join(format!("workspace-{id}"));
    std::fs::create_dir_all(&path).unwrap();
    workspace::WorkspaceKey::canonical("node", "requester", "agent", &path).unwrap()
}

fn binding(root: &Path, value: &policy::Grant<u64, u64>) -> workspace::Binding {
    policy::Grant {
        scope: policy::Scope {
            workspace: key(root, value.scope.workspace),
            revision: value.scope.revision,
        },
        action: workspace::BindingAction::Command {
            argv: vec!["printf".into(), format!("fixture-{}", value.action)],
            cwd: root.to_owned(),
            root: root.to_owned(),
            writable_root: Some(root.to_owned()),
            policy: crate::toolset::CommandExecutionPolicy::write_capable(),
        },
    }
}

#[derive(Debug, Deserialize)]
struct QuestionState {
    present: bool,
    decision: Option<bool>,
}
#[derive(Debug, Deserialize)]
struct State {
    question: QuestionState,
    grants: Vec<policy::Grant<u64, u64>>,
}
#[derive(Debug, Deserialize)]
struct Reuse {
    candidate: policy::Grant<u64, u64>,
    allowed: bool,
}
#[derive(Debug, Deserialize)]
struct AnswerCase {
    available: Vec<policy::Scope<u64>>,
    binding: policy::Grant<u64, u64>,
    initial: State,
    id: String,
    now: u64,
    choice: Answer,
    kind: policy::ApprovalKind,
    supported: bool,
    accepted: bool,
    expected: State,
    reuse: Vec<Reuse>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RevokeCase {
    previous: policy::Preference<u64>,
    expected_revision: u64,
    grants: Vec<policy::Grant<u64, u64>>,
    result: RevokeResult,
    remaining: Vec<policy::Grant<u64, u64>>,
}
#[derive(Deserialize)]
struct RevokeResult {
    accepted: bool,
    changed: bool,
    next: Option<policy::Preference<u64>>,
}

fn store(home: &Path, entries: &[workspace::Settings]) {
    std::fs::write(
        workspace::preferences_file(home),
        serde_json::to_vec(&serde_json::json!({ "entries": entries })).unwrap(),
    )
    .unwrap();
}

#[test]
fn generated_command_answers_preserve_scope_and_one_time_delivery() {
    let snapshot = crate::lean_vocab_test::lean_contract_snapshot();
    assert_eq!(snapshot.workspace_command_grant_answers.len(), 962);
    assert_eq!(snapshot.workspace_command_grant_lookups.len(), 24);
    for (index, value) in snapshot.workspace_command_grant_answers.iter().enumerate() {
        let case: AnswerCase = serde_json::from_value(value.clone()).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        assert!(case.initial.grants.is_empty());
        let bound = binding(&root, &case.binding);
        let entries: Vec<_> = case
            .available
            .iter()
            .map(|scope| workspace::Settings {
                workspace: key(&root, scope.workspace),
                selection: policy::Preference {
                    revision: scope.revision,
                    mode: policy::Mode::Ask,
                    review_task: None,
                },
                dirs: Vec::new(),
                command_grants: Vec::new(),
            })
            .collect();
        store(&home, &entries);
        let mut request = Request::for_binding(bound.clone(), None);
        request.id = "q1".into();
        request.created_at_ms = 10;
        request.expires_at_ms = 20;
        let mut wire = serde_json::to_value(&request).unwrap();
        wire["command_grant"] = case.supported.into();
        if case.kind != policy::ApprovalKind::Command {
            wire["binding"]["action"] = serde_json::json!({
                "kind": "fileMutation", "tool": "write_file", "inputDigest": "fixture",
                "path": root.join("document.txt"), "root": root, "existed": false,
            });
        }
        let request: Request = serde_json::from_value(wire).unwrap();
        if case.initial.question.present {
            write_atomic(
                &Queue::Workspace.request_file(&home, "q1"),
                &serde_json::to_vec(&request).unwrap(),
            )
            .unwrap();
        }
        if let Some(allow) = case.initial.question.decision {
            write_atomic(
                &Queue::Workspace.decision_file(&home, "q1"),
                &serde_json::to_vec(&Decision {
                    allow,
                    binding: Some(bound.clone()),
                })
                .unwrap(),
            )
            .unwrap();
        }
        assert_eq!(
            decide_with_clock(&home, &case.id, case.choice, || case.now).is_ok(),
            case.accepted,
            "answer case {index}: {case:?}"
        );
        let observed = std::fs::read(Queue::Workspace.decision_file(&home, "q1"))
            .ok()
            .map(|bytes| serde_json::from_slice::<Decision>(&bytes).unwrap().allow);
        assert_eq!(
            observed, case.expected.question.decision,
            "answer case {index}"
        );
        assert_eq!(
            Queue::Workspace.request_file(&home, "q1").exists(),
            case.expected.question.present
        );
        let settings = workspace::get(&home, &bound.scope.workspace).unwrap();
        assert_eq!(
            settings
                .as_ref()
                .map_or(0, |entry| entry.command_grants.len()),
            case.expected.grants.len(),
            "answer case {index}"
        );
        for query in case.reuse {
            let candidate = binding(&root, &query.candidate);
            assert_eq!(
                settings
                    .as_ref()
                    .is_some_and(|entry| workspace::has_command_grant(entry, &candidate).unwrap()),
                query.allowed,
                "reuse case {index}"
            );
        }
        let stored = std::fs::read_to_string(workspace::preferences_file(&home)).unwrap();
        assert!(
            !stored.contains("fixture-"),
            "command arguments must not be persisted"
        );
        assert!(allowed::list(&home).unwrap().is_empty());
    }
}

#[test]
fn generated_command_revocation_preserves_the_selected_mode() {
    let snapshot = crate::lean_vocab_test::lean_contract_snapshot();
    assert_eq!(snapshot.workspace_command_grant_revocations.len(), 54);
    for value in &snapshot.workspace_command_grant_revocations {
        let case: RevokeCase = serde_json::from_value(value.clone()).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let workspace = key(&root, 1);
        let review_task = case.previous.review_task.map(|_| {
            workspace::ReviewTaskSelection::for_task(&workspace::default_review_task(
                "node", "agent",
            ))
        });
        let settings = workspace::Settings {
            workspace: workspace.clone(),
            selection: policy::Preference {
                revision: case.previous.revision,
                mode: case.previous.mode,
                review_task: review_task.clone(),
            },
            dirs: vec![allowed::AllowedDir {
                path: root.clone(),
                access: BindAccess::Read,
            }],
            command_grants: case
                .grants
                .iter()
                .map(|value| workspace::command_grant(&binding(&root, value)).unwrap())
                .collect(),
        };
        store(&root, &[settings]);
        let result = workspace::revoke_commands(&root, &workspace, case.expected_revision);
        assert_eq!(result.is_ok(), case.result.accepted);
        assert_eq!(case.result.changed, case.result.accepted);
        let observed = workspace::get(&root, &workspace).unwrap().unwrap();
        let expected = case.result.next.unwrap();
        assert_eq!(observed.selection.revision, expected.revision);
        assert_eq!(observed.selection.mode, expected.mode);
        assert_eq!(observed.selection.review_task, review_task);
        assert_eq!(observed.command_grants.len(), case.remaining.len());
        assert_eq!(observed.dirs.len(), 1);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn automatic_review_reuses_remembered_invocations_until_revocation() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let key = workspace::WorkspaceKey::canonical("node", "requester", "agent", &project).unwrap();
    let reviewer =
        workspace::ReviewTaskSelection::for_task(&workspace::default_review_task("node", "agent"));
    workspace::save_mode(&home, &key, 0, policy::Mode::AutoReview, Some(&reviewer)).unwrap();
    mark_listening(&home).unwrap();
    let context = workspace::for_request(
        Some(&home),
        "node",
        Some("requester"),
        "agent",
        Some(&project),
    )
    .unwrap();
    workspace::scope(
        context,
        scope_interactive(true, async {
            let tools = crate::toolset::ToolContext::new(project.clone(), false).unwrap();
            let policy = crate::toolset::CommandExecutionPolicy::write_capable();
            let args = vec!["remembered-invocation".to_owned()];
            let run = || {
                crate::toolset::run_command_for_test(
                    &tools,
                    "bash",
                    "printf",
                    &args,
                    None,
                    Duration::from_secs(10),
                    &policy,
                    false,
                    1024,
                )
            };
            let mut first = Box::pin(run());
            assert!(futures::poll!(first.as_mut()).is_pending());
            let questions = pending(&home).unwrap();
            assert_eq!(questions.len(), 1);
            assert!(questions[0].allows_answer(Answer::AlwaysInvocation));
            decide(&home, &questions[0].id, Answer::AlwaysInvocation).unwrap();
            assert!(first.await.unwrap().contains("remembered-invocation"));
            assert!(run().await.unwrap().contains("remembered-invocation"));
            assert!(pending_requests(&home).unwrap().is_empty());
            let current = workspace::get(&home, &key).unwrap().unwrap();
            workspace::revoke_commands(&home, &key, current.selection.revision).unwrap();
            let mut again = Box::pin(run());
            assert!(futures::poll!(again.as_mut()).is_pending());
            let questions = pending(&home).unwrap();
            assert_eq!(questions.len(), 1);
            decide(&home, &questions[0].id, Answer::Once).unwrap();
            assert!(again.await.unwrap().contains("remembered-invocation"));
            let mut once_more = Box::pin(run());
            assert!(futures::poll!(once_more.as_mut()).is_pending());
            let questions = pending(&home).unwrap();
            assert_eq!(questions.len(), 1);
            decide(&home, &questions[0].id, Answer::Deny).unwrap();
            assert!(once_more.await.is_err());
        }),
    )
    .await;
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn one_time_workspace_approval_creates_a_profile_only_inside_its_write_root() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let home = root.join("runtime");
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let key = workspace::WorkspaceKey::canonical("node", "requester", "agent", &project).unwrap();
    workspace::save_mode(&home, &key, 0, policy::Mode::Ask, None).unwrap();
    mark_listening(&home).unwrap();
    let context = workspace::for_request(
        Some(&home),
        "node",
        Some("requester"),
        "agent",
        Some(&project),
    )
    .unwrap();
    workspace::scope(
        context,
        scope_interactive(true, async {
            let tools = crate::toolset::ToolContext::new(project.clone(), false).unwrap();
            let policy = crate::toolset::CommandExecutionPolicy::write_capable()
                .with_mode(crate::toolset::CommandExecutionMode::WorkspaceWrite);
            let args = vec![
                "-c".into(),
                concat!(
                    "test \"$TMPDIR\" = \"$TMP\" && test \"$TMP\" = \"$TEMP\" && ",
                    "/usr/bin/mktemp -d \"$TMPDIR/browser-profile-XXXXXX\"",
                )
                .into(),
            ];
            let run = || {
                crate::toolset::run_command_for_test(
                    &tools,
                    "bash",
                    "/bin/sh",
                    &args,
                    None,
                    Duration::from_secs(10),
                    &policy,
                    false,
                    4096,
                )
            };
            let mut first = Box::pin(run());
            assert!(futures::poll!(first.as_mut()).is_pending());
            let questions = pending(&home).unwrap();
            assert_eq!(questions.len(), 1);
            let original = questions[0].id.clone();
            decide(&home, &original, Answer::Once).unwrap();
            let result = first.await.unwrap();
            let temporary = project.join(".gents-tmp");
            let profiles: Vec<_> = std::fs::read_dir(&temporary)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect();
            assert_eq!(
                profiles.len(),
                1,
                "approved command did not create one profile: {result}"
            );
            assert!(profiles[0].is_dir());
            assert!(
                result.contains(&profiles[0].to_string_lossy().to_string()),
                "{result}"
            );
            assert!(pending_requests(&home).unwrap().is_empty());
            let mut again = Box::pin(run());
            assert!(futures::poll!(again.as_mut()).is_pending());
            let questions = pending(&home).unwrap();
            assert_eq!(questions.len(), 1);
            assert_ne!(questions[0].id, original);
            decide(&home, &questions[0].id, Answer::Deny).unwrap();
            assert!(again.await.is_err());
            assert_eq!(std::fs::read_dir(&temporary).unwrap().count(), 1);
            assert!(pending_requests(&home).unwrap().is_empty());
        }),
    )
    .await;
}

#[test]
fn invocation_digest_covers_every_command_boundary_and_old_producers_offer_no_remember_choice() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let bound = binding(
        &root,
        &policy::Grant {
            scope: policy::Scope {
                workspace: 1,
                revision: 2,
            },
            action: 1,
        },
    );
    let original = workspace::command_grant(&bound).unwrap();
    let settings = workspace::Settings {
        workspace: bound.scope.workspace.clone(),
        selection: policy::Preference {
            revision: 2,
            mode: policy::Mode::Ask,
            review_task: None,
        },
        dirs: Vec::new(),
        command_grants: vec![original],
    };
    let bytes = serde_json::to_vec(&bound).unwrap();
    let alterations = [
        ("argv", serde_json::json!(["printf", "changed"])),
        ("argv", serde_json::json!(["/usr/bin/printf", "fixture-1"])),
        ("cwd", serde_json::json!(root.join("sub"))),
        ("root", serde_json::json!(root.join("other"))),
        ("writableRoot", serde_json::Value::Null),
    ];
    for (field, value) in alterations {
        let mut changed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        changed["action"][field] = value;
        assert!(
            !workspace::has_command_grant(&settings, &serde_json::from_value(changed).unwrap())
                .unwrap()
        );
    }
    for (field, value) in [
        ("mode", serde_json::json!("read_only")),
        ("networkMode", serde_json::json!("disabled")),
        ("allowedArgvPrefixes", serde_json::json!([["printf"]])),
        ("forbiddenArgvPrefixes", serde_json::json!([["rm"]])),
        ("readOnlyAllowlist", serde_json::json!(["printf"])),
        ("denyAllArgv", serde_json::json!(true)),
        ("denyGitMetadataWrites", serde_json::json!(true)),
    ] {
        let mut changed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        changed["action"]["policy"][field] = value;
        let changed = serde_json::from_value(changed).unwrap();
        assert!(
            !workspace::has_command_grant(&settings, &changed).unwrap(),
            "{field}"
        );
    }
    let request = Request::for_binding(bound, None);
    let mut old = serde_json::to_value(request).unwrap();
    old.as_object_mut().unwrap().remove("command_grant");
    let old: Request = serde_json::from_value(old).unwrap();
    assert!(!old.allows_answer(Answer::AlwaysInvocation));
    assert!(old.allows_answer(Answer::Once));
}
