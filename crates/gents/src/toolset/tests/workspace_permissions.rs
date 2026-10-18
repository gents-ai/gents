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
async fn read_only_commands_and_full_access_do_not_prompt() {
    for mode in [workspace::Mode::Ask, workspace::Mode::FullAccess] {
        let fixture = Fixture::new();
        fixture.select(mode);
        let policy = if mode == workspace::Mode::Ask {
            CommandExecutionPolicy::read_only(vec!["echo".into()])
        } else {
            write_policy()
        };
        let result = fixture
            .run(policy, "/bin/echo", &["approved".into()])
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
