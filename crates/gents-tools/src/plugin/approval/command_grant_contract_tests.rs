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
            policy: crate::native::CommandExecutionPolicy::write_capable(),
        },
    }
}

fn store(home: &Path, entries: &[workspace::Settings]) {
    std::fs::write(
        workspace::preferences_file(home),
        serde_json::to_vec(&serde_json::json!({ "entries": entries })).unwrap(),
    )
    .unwrap();
}

#[test]
fn workspace_command_grants_answers_match_lean() {
    use gents_lean_vocab::families::workspace_command_grants_answers::{
        AnswerOutput, QuestionWire, Reuse, StateWire,
    };
    for case in gents_lean_vocab::families::workspace_command_grants_answers::cases() {
        let input = &case.input;
        let binding_grant: policy::Grant<u64, u64> =
            gents_lean_vocab::family::recode(&input.binding);
        let available: Vec<policy::Scope<u64>> = gents_lean_vocab::family::recode(&input.available);
        let candidates: Vec<policy::Grant<u64, u64>> =
            gents_lean_vocab::family::recode(&input.candidates);
        let choice: Answer =
            serde_json::from_value(serde_json::Value::String(input.choice.clone())).unwrap();
        let kind: policy::ApprovalKind =
            serde_json::from_value(serde_json::Value::String(input.kind.clone())).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        assert!(input.initial.grants.is_empty(), "{input:?}");
        let bound = binding(&root, &binding_grant);
        let entries: Vec<_> = available
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
        wire["command_grant"] = input.supported.into();
        if kind != policy::ApprovalKind::Command {
            wire["binding"]["action"] = serde_json::json!({
                "kind": "fileMutation", "tool": "write_file", "inputDigest": "fixture",
                "path": root.join("document.txt"), "root": root, "existed": false,
            });
        }
        let request: Request = serde_json::from_value(wire).unwrap();
        if input.initial.question.present {
            write_atomic(
                &Queue::Workspace.request_file(&home, "q1"),
                &serde_json::to_vec(&request).unwrap(),
            )
            .unwrap();
        }
        if let Some(allow) = input.initial.question.decision {
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
        let accepted = decide_with_clock(&home, &input.id, choice, || input.now).is_ok();
        let decision = std::fs::read(Queue::Workspace.decision_file(&home, "q1"))
            .ok()
            .map(|bytes| serde_json::from_slice::<Decision>(&bytes).unwrap().allow);
        let present = Queue::Workspace.request_file(&home, "q1").exists();
        let settings = workspace::resolve(&home, &bound.scope.workspace, None).unwrap();
        let granted = |candidate: &policy::Grant<u64, u64>| {
            settings.as_ref().is_some_and(|entry| {
                workspace::has_command_grant(entry, &binding(&root, candidate)).unwrap()
            })
        };
        let grants: Vec<policy::Grant<u64, u64>> = [binding_grant.clone()]
            .into_iter()
            .filter(|grant| granted(grant))
            .collect();
        assert_eq!(
            settings
                .as_ref()
                .map_or(0, |entry| entry.command_grants.len()),
            grants.len(),
            "{input:?}"
        );
        let reuse = candidates
            .iter()
            .map(|candidate| Reuse {
                candidate: gents_lean_vocab::family::recode(candidate),
                allowed: granted(candidate),
            })
            .collect();
        let stored = std::fs::read_to_string(workspace::preferences_file(&home)).unwrap();
        assert!(
            !stored.contains("fixture-"),
            "command arguments must not be persisted"
        );
        assert!(allowed::list(&home).unwrap().is_empty());
        case.check(AnswerOutput {
            accepted,
            expected: StateWire {
                question: QuestionWire { present, decision },
                grants: gents_lean_vocab::family::recode(&grants),
            },
            reuse,
        });
    }
}

#[test]
fn workspace_command_grants_revocations_match_lean() {
    use gents_lean_vocab::families::workspace_command_grants_revocations::{
        PreferenceWire, RevokeOutput, RevokeResult,
    };
    for case in gents_lean_vocab::families::workspace_command_grants_revocations::cases() {
        let input = &case.input;
        let mode: policy::Mode =
            serde_json::from_value(serde_json::Value::String(input.previous.mode.clone())).unwrap();
        let grants: Vec<policy::Grant<u64, u64>> = gents_lean_vocab::family::recode(&input.grants);
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let workspace = key(&root, 1);
        let review_task = input.previous.review_task.map(|_| {
            workspace::ReviewTaskSelection::for_task(&workspace::default_review_task(
                "node", "agent",
            ))
        });
        let stored_grants: Vec<_> = grants
            .iter()
            .map(|value| workspace::command_grant(&binding(&root, value)).unwrap())
            .collect();
        let settings = workspace::Settings {
            workspace: workspace.clone(),
            selection: policy::Preference {
                revision: input.previous.revision,
                mode,
                review_task: review_task.clone(),
            },
            dirs: vec![allowed::AllowedDir {
                path: root.clone(),
                access: BindAccess::Read,
            }],
            command_grants: stored_grants.clone(),
        };
        store(&root, &[settings]);
        let accepted =
            workspace::revoke_commands(&root, &workspace, input.expected_revision).is_ok();
        let observed = workspace::resolve(&root, &workspace, None)
            .unwrap()
            .unwrap();
        assert_eq!(observed.dirs.len(), 1, "{input:?}");
        let observed_grants: Vec<_> = observed
            .command_grants
            .iter()
            .map(|grant| serde_json::to_value(grant).unwrap())
            .collect();
        let remaining: Vec<policy::Grant<u64, u64>> = grants
            .iter()
            .zip(&stored_grants)
            .filter(|(_, stored)| observed_grants.contains(&serde_json::to_value(stored).unwrap()))
            .map(|(grant, _)| grant.clone())
            .collect();
        assert_eq!(observed.command_grants.len(), remaining.len(), "{input:?}");
        let review_task_observed = match (&observed.selection.review_task, &review_task) {
            (None, _) => None,
            (Some(observed), Some(selected)) if observed == selected => input.previous.review_task,
            (Some(observed), _) => panic!("{input:?}: unexpected review task {observed:?}"),
        };
        let mode = serde_json::to_value(observed.selection.mode).unwrap();
        let next = PreferenceWire {
            revision: observed.selection.revision,
            mode: mode.as_str().unwrap().to_owned(),
            review_task: review_task_observed,
        };
        case.check(RevokeOutput {
            result: RevokeResult {
                accepted,
                changed: observed.selection.revision != input.previous.revision,
                next: Some(next),
            },
            remaining: gents_lean_vocab::family::recode(&remaining),
        });
    }
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn automatic_review_reuses_remembered_invocations_until_revocation() {
    remembered_invocations_until_revocation(true).await;
}

#[cfg(unix)]
#[tokio::test]
async fn automatic_review_admission_reuses_remembered_invocations_until_revocation() {
    remembered_invocations_until_revocation(false).await;
}

#[cfg(unix)]
async fn remembered_invocations_until_revocation(launch: bool) {
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
        None,
    )
    .unwrap();
    workspace::scope(
        context,
        scope_interactive(true, async {
            let tools = crate::native::ToolContext::new(project.clone(), false).unwrap();
            let policy = crate::native::CommandExecutionPolicy::write_capable();
            let args = vec!["remembered-invocation".to_owned()];
            let argv = vec!["printf".to_owned(), args[0].clone()];
            let run = || async {
                if launch {
                    let output = crate::native::run_command_for_test(
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
                    .await?;
                    assert!(output.contains("remembered-invocation"));
                    Ok::<(), anyhow::Error>(())
                } else {
                    workspace::authorize_command(&argv, &project, &project, None, &policy).await
                }
            };
            let (operator, observation) =
                crate::plugin::approval::testing::Operator::answer_with_observation(
                    &home,
                    Answer::AlwaysInvocation,
                )
                .await
                .unwrap();
            let (first, prompt) = tokio::join!(run(), operator);
            let prompt = prompt.unwrap();
            assert!(!observation.id().unwrap().is_empty());
            assert!(!prompt.is_empty());
            first.unwrap();
            run().await.unwrap();
            assert!(pending_requests(&home).unwrap().is_empty());
            let current = workspace::resolve(&home, &key, None).unwrap().unwrap();
            workspace::revoke_commands(&home, &key, current.selection.revision).unwrap();
            let operator = crate::plugin::approval::testing::Operator::answer(&home, Answer::Once)
                .await
                .unwrap();
            let (again, observed) = tokio::join!(run(), operator);
            observed.unwrap();
            again.unwrap();
            let operator = crate::plugin::approval::testing::Operator::answer(&home, Answer::Deny)
                .await
                .unwrap();
            let (once_more, observed) = tokio::join!(run(), operator);
            observed.unwrap();
            assert!(once_more.is_err());
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
        None,
    )
    .unwrap();
    workspace::scope(
        context,
        scope_interactive(true, async {
            let tools = crate::native::ToolContext::new(project.clone(), false).unwrap();
            let policy = crate::native::CommandExecutionPolicy::write_capable()
                .with_mode(crate::native::CommandExecutionMode::WorkspaceWrite);
            let args = vec![
                "-c".into(),
                concat!(
                    "test \"$TMPDIR\" = \"$TMP\" && test \"$TMP\" = \"$TEMP\" && ",
                    "/usr/bin/mktemp -d \"$TMPDIR/browser-profile-XXXXXX\"",
                )
                .into(),
            ];
            let run = || {
                crate::native::run_command_for_test(
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
            let (operator, observation) =
                crate::plugin::approval::testing::Operator::answer_with_observation(
                    &home,
                    Answer::Once,
                )
                .await
                .unwrap();
            let (result, prompt) = tokio::join!(run(), operator);
            let prompt = prompt.unwrap();
            assert!(!prompt.is_empty());
            let original = observation.id().unwrap();
            let result = result.unwrap();
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
            let (operator, next_observation) =
                crate::plugin::approval::testing::Operator::answer_with_observation(
                    &home,
                    Answer::Deny,
                )
                .await
                .unwrap();
            let (result, observed) = tokio::join!(run(), operator);
            let observed = observed.unwrap();
            assert!(!observed.is_empty());
            // A one-time answer is deliberately not remembered; a later
            // invocation must publish a distinct approval identity.
            assert_ne!(next_observation.id().unwrap(), original);
            assert!(result.is_err());
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
        assert!(!workspace::has_command_grant(
            &settings,
            &serde_json::from_value(changed).unwrap()
        )
        .unwrap());
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

#[cfg(all(unix, not(target_os = "macos")))]
#[tokio::test]
async fn guarded_command_launch_rejects_unsupported_isolation_before_asking() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let home = root.join("home");
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
        None,
    )
    .unwrap();
    let tools = crate::native::ToolContext::new(project.clone(), false).unwrap();
    let command_policy = crate::native::CommandExecutionPolicy::write_capable()
        .with_mode(crate::native::CommandExecutionMode::Unrestricted);
    let marker = project.join("must-not-run");
    let args = vec![
        "-c".into(),
        "printf unexpected > \"$1\"".into(),
        "guard-fixture".into(),
        marker.to_string_lossy().into_owned(),
    ];
    let error = workspace::scope(
        context,
        scope_interactive(
            true,
            crate::native::run_command_for_test(
                &tools,
                "bash_unrestricted",
                "/bin/sh",
                &args,
                None,
                Duration::from_secs(1),
                &command_policy,
                false,
                1024,
            ),
        ),
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Command isolation for runtime permissions"),
        "{error:#}"
    );
    assert!(!marker.exists());
    assert!(pending_requests(&home).unwrap().is_empty());
}
