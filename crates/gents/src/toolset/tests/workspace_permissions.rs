use super::*;
use crate::llm::tool::Tool;
use crate::plugin::approval::{self, workspace, Answer, Request};

struct Fixture {
    home: tempfile::TempDir,
    root: tempfile::TempDir,
    key: workspace::WorkspaceKey,
}

impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let key =
            workspace::WorkspaceKey::canonical("node", "requester", "agent", root.path()).unwrap();
        Self { home, root, key }
    }

    fn select(&self, mode: workspace::Mode) {
        workspace::save_mode(self.home.path(), &self.key, 0, mode, None).unwrap();
    }

    async fn run(
        &self,
        policy: CommandExecutionPolicy,
        command: &str,
        args: &[String],
    ) -> Result<String, ToolError> {
        let context = workspace::for_request(
            Some(self.home.path()),
            "node",
            Some("requester"),
            "agent",
            Some(self.root.path()),
        )
        .unwrap();
        let tool = UnrestrictedBashTool::with_policy(
            ToolContext::new(self.root.path().to_owned(), false).unwrap(),
            Duration::from_secs(2),
            Duration::from_secs(2),
            1024,
            policy,
        );
        workspace::scope(
            context,
            approval::scope_interactive(
                true,
                Tool::call(
                    &tool,
                    BashArgs {
                        command: command.to_owned(),
                        args: args.to_vec(),
                        cwd: None,
                        timeout_secs: None,
                        raw_json: true,
                    },
                ),
            ),
        )
        .await
    }

    async fn question(&self) -> Request {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(question) = approval::pending_requests(self.home.path()).unwrap().pop()
                {
                    return question;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the command never requested consent")
    }

    fn marker(&self) -> String {
        self.key
            .path
            .join("executed")
            .to_string_lossy()
            .into_owned()
    }
}

fn write_policy() -> CommandExecutionPolicy {
    CommandExecutionPolicy::write_capable().with_mode(CommandExecutionMode::Unrestricted)
}

#[tokio::test]
async fn command_consent_precedes_execution_and_cannot_remember_a_folder() {
    let fixture = Fixture::new();
    fixture.select(workspace::Mode::Ask);
    approval::mark_listening(fixture.home.path()).unwrap();
    let args = [fixture.marker()];
    let call = fixture.run(write_policy(), "/usr/bin/touch", &args);
    let answer = async {
        let question = fixture.question().await;
        assert!(!PathBuf::from(fixture.marker()).exists());
        let command = question.command().unwrap();
        assert_eq!(
            command.argv,
            ["/usr/bin/touch".to_owned(), fixture.marker()]
        );
        assert_eq!(command.cwd, fixture.key.path);
        assert_eq!(command.policy, &write_policy());
        assert!(approval::decide(fixture.home.path(), &question.id, Answer::AlwaysFolder).is_err());
        assert!(!PathBuf::from(fixture.marker()).exists());
        approval::decide(fixture.home.path(), &question.id, Answer::Once).unwrap();
    };
    let (result, ()) = tokio::join!(call, answer);
    result.unwrap();
    assert!(PathBuf::from(fixture.marker()).is_file());
    assert!(approval::pending_requests(fixture.home.path())
        .unwrap()
        .is_empty());
    assert!(workspace::get(fixture.home.path(), &fixture.key)
        .unwrap()
        .unwrap()
        .dirs
        .is_empty());
}

#[tokio::test]
async fn denied_command_cannot_execute() {
    let fixture = Fixture::new();
    fixture.select(workspace::Mode::Ask);
    approval::mark_listening(fixture.home.path()).unwrap();
    let args = [fixture.marker()];
    let call = fixture.run(write_policy(), "/usr/bin/touch", &args);
    let answer = async {
        let question = fixture.question().await;
        approval::decide(fixture.home.path(), &question.id, Answer::Deny).unwrap();
    };
    let (result, ()) = tokio::join!(call, answer);
    assert!(matches!(
        result,
        Err(ToolError::ReportedFailure {
            class: crate::tool_call_lifecycle::FailureClass::PolicyDenied,
            ..
        })
    ));
    assert!(!PathBuf::from(fixture.marker()).exists());
}

#[tokio::test]
async fn changed_workspace_selection_retires_the_command_without_execution() {
    let fixture = Fixture::new();
    fixture.select(workspace::Mode::Ask);
    approval::mark_listening(fixture.home.path()).unwrap();
    let args = [fixture.marker()];
    let call = fixture.run(write_policy(), "/usr/bin/touch", &args);
    let answer = async {
        let question = fixture.question().await;
        workspace::save_mode(
            fixture.home.path(),
            &fixture.key,
            1,
            workspace::Mode::FullAccess,
            None,
        )
        .unwrap();
        assert!(approval::decide(fixture.home.path(), &question.id, Answer::Once).is_err());
    };
    let (result, ()) = tokio::join!(call, answer);
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("permissions changed"));
    assert!(!PathBuf::from(fixture.marker()).exists());
    assert!(approval::pending_requests(fixture.home.path())
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn full_access_preserves_the_configured_command_ceiling() {
    let fixture = Fixture::new();
    fixture.select(workspace::Mode::FullAccess);
    let args = [fixture.marker()];
    let policy = write_policy().with_forbidden_argv_prefixes(vec![vec!["/usr/bin/touch".into()]]);
    let result = fixture.run(policy, "/usr/bin/touch", &args).await;
    assert!(matches!(result, Err(ToolError::PolicyDenial(_))));
    assert!(!PathBuf::from(fixture.marker()).exists());
    assert!(approval::pending_requests(fixture.home.path())
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn cancelling_a_command_wait_removes_its_question_without_execution() {
    let fixture = Fixture::new();
    fixture.select(workspace::Mode::Ask);
    approval::mark_listening(fixture.home.path()).unwrap();
    let args = [fixture.marker()];
    tokio::select! {
        result = fixture.run(write_policy(), "/usr/bin/touch", &args) => {
            panic!("the command finished before receiving consent: {result:?}");
        }
        _ = fixture.question() => {}
    }
    assert!(!PathBuf::from(fixture.marker()).exists());
    assert!(approval::pending_requests(fixture.home.path())
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn strict_ask_requests_consent_for_each_read_only_command() {
    let fixture = Fixture::new();
    fixture.select(workspace::Mode::Ask);
    approval::mark_listening(fixture.home.path()).unwrap();
    let argv = ["approved".into()];
    for _ in 0..2 {
        let call = fixture.run(
            CommandExecutionPolicy::read_only(vec!["echo".into()]),
            "/bin/echo",
            &argv,
        );
        let answer = async {
            let question = fixture.question().await;
            assert_eq!(question.command().unwrap().argv, ["/bin/echo", "approved"]);
            approval::decide(fixture.home.path(), &question.id, Answer::Once).unwrap();
        };
        let (result, ()) = tokio::join!(call, answer);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&result.unwrap()).unwrap()["stdout"],
            "approved\n"
        );
    }
}

#[tokio::test]
async fn full_access_commands_do_not_prompt() {
    let fixture = Fixture::new();
    fixture.select(workspace::Mode::FullAccess);
    let result = fixture
        .run(write_policy(), "/bin/echo", &["approved".into()])
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&result).unwrap()["stdout"],
        "approved\n"
    );
    assert!(approval::pending_requests(fixture.home.path())
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn callers_without_workspace_preferences_keep_their_configured_policy() {
    let fixture = Fixture::new();
    let result = fixture
        .run(write_policy(), "/bin/echo", &["approved".into()])
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&result).unwrap()["stdout"],
        "approved\n"
    );
    assert!(approval::pending_requests(fixture.home.path())
        .unwrap()
        .is_empty());
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn workspace_command_sandbox_tracks_the_selected_permission_mode() {
    let fixture = Fixture::new();
    let project = fixture.root.path().join("project");
    let sibling = fixture.root.path().join("sibling");
    std::fs::create_dir(&project).unwrap();
    std::fs::create_dir(&sibling).unwrap();
    let key = workspace::WorkspaceKey::canonical("node", "requester", "agent", &project).unwrap();
    workspace::save_mode(fixture.home.path(), &key, 0, workspace::Mode::Ask, None).unwrap();
    let scope = workspace::for_request(
        Some(fixture.home.path()),
        "node",
        Some("requester"),
        "agent",
        Some(&project),
    )
    .unwrap();
    let context =
        ToolContext::new_with_base(fixture.root.path().to_owned(), Some(project.clone()), false)
            .unwrap();
    let tool = UnrestrictedBashTool::with_policy(
        context,
        Duration::from_secs(5),
        Duration::from_secs(5),
        1024,
        CommandExecutionPolicy::write_capable()
            .with_mode(CommandExecutionMode::WorkspaceWrite)
            .with_network_mode(CommandNetworkMode::Disabled),
    );
    approval::mark_listening(fixture.home.path()).unwrap();
    workspace::scope(
        scope,
        approval::scope_interactive(true, async {
            for (directory, allowed) in [(&project, true), (&sibling, false)] {
                let marker = directory.join("executed");
                let call = Tool::call(
                    &tool,
                    BashArgs {
                        command: "/usr/bin/touch".into(),
                        args: vec![marker.to_str().unwrap().into()],
                        cwd: Some(directory.to_str().unwrap().into()),
                        timeout_secs: None,
                        raw_json: true,
                    },
                );
                let answer = async {
                    let question = fixture.question().await;
                    let workspace::BindingAction::Command { writable_root, .. } =
                        &question.binding().unwrap().action
                    else {
                        panic!("expected a command approval")
                    };
                    assert_eq!(writable_root.as_deref(), Some(key.path.as_path()));
                    approval::decide(fixture.home.path(), &question.id, Answer::Once).unwrap();
                };
                let (result, ()) = tokio::join!(call, answer);
                let output: serde_json::Value = match result {
                    Ok(output) if allowed => serde_json::from_str(&output).unwrap(),
                    Err(ToolError::ReportedFailure {
                        class: crate::tool_call_lifecycle::FailureClass::ToolReturnedError,
                        text,
                    }) if !allowed => serde_json::from_str(&text).unwrap(),
                    other => panic!("unexpected command result: {other:?}"),
                };
                assert_eq!(output["ok"], allowed, "{output}");
                assert_eq!(marker.exists(), allowed);
            }
            workspace::save_mode(
                fixture.home.path(),
                &key,
                1,
                workspace::Mode::FullAccess,
                None,
            )
            .unwrap();
            let marker = sibling.join("executed");
            Tool::call(
                &tool,
                BashArgs {
                    command: "/usr/bin/touch".into(),
                    args: vec![marker.to_str().unwrap().into()],
                    cwd: Some(sibling.to_str().unwrap().into()),
                    timeout_secs: None,
                    raw_json: true,
                },
            )
            .await
            .unwrap();
            assert!(marker.is_file());
        }),
    )
    .await;
    assert!(approval::pending_requests(fixture.home.path())
        .unwrap()
        .is_empty());
}
