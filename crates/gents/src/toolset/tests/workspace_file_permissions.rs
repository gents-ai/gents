use std::future::Future;
use std::path::{Path, PathBuf};

use super::*;
use crate::inference::llm::tool::Tool;
use crate::plugin::approval::{self, workspace, Answer, Request};
use crate::toolset::file_tools::{content_hash, file_mutation_lock_for};

struct Fixture {
    home: tempfile::TempDir,
    root: tempfile::TempDir,
    key: workspace::WorkspaceKey,
}

impl Fixture {
    fn new(mode: Option<workspace::Mode>) -> Self {
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("workspace");
        std::fs::create_dir(&path).unwrap();
        let key = workspace::WorkspaceKey::canonical("node", "requester", "agent", &path).unwrap();
        if let Some(mode) = mode {
            workspace::save_mode(home.path(), &key, 0, mode, None).unwrap();
        }
        approval::mark_listening(home.path()).unwrap();
        Self { home, root, key }
    }

    fn outside(&self) -> PathBuf {
        self.root
            .path()
            .canonicalize()
            .unwrap()
            .join("other/nested/file.txt")
    }

    async fn run<F>(&self, future: F) -> F::Output
    where
        F: Future,
    {
        let context = workspace::for_request(
            Some(self.home.path()),
            "node",
            Some("requester"),
            "agent",
            Some(&self.key.path),
        )
        .unwrap();
        tokio::time::timeout(
            Duration::from_secs(30),
            workspace::scope(context, approval::scope_interactive(true, future)),
        )
        .await
        .expect("file operation did not finish")
    }

    async fn write(&self, path: &Path) -> Result<String, ToolError> {
        let tool =
            WriteFileTool::new(ToolContext::new(self.root.path().to_owned(), false).unwrap());
        self.run(Tool::call(
            &tool,
            WriteFileArgs {
                path: path.to_str().unwrap().into(),
                content: "hello world".into(),
                raw_json: true,
                expected_content_hash: None,
                overwrite: false,
            },
        ))
        .await
    }

    async fn edit(
        &self,
        path: &Path,
        dry_run: bool,
        hash: Option<String>,
    ) -> Result<String, ToolError> {
        let tool = EditFileTool::new(ToolContext::new(self.root.path().to_owned(), false).unwrap());
        self.run(Tool::call(
            &tool,
            EditFileArgs {
                path: path.to_str().unwrap().into(),
                old_text: "world".into(),
                new_text: "reader".into(),
                replace_all: false,
                raw_json: true,
                dry_run,
                match_mode: None,
                operation: None,
                expected_content_hash: hash,
            },
        ))
        .await
    }

    async fn question(&self) -> Request {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(question) = approval::pending(self.home.path()).unwrap().pop() {
                    return question;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("file operation did not request consent")
    }

    fn assert_clear(&self) {
        assert!(approval::pending_requests(self.home.path())
            .unwrap()
            .is_empty());
        assert!(crate::plugin::allowed::list(self.home.path())
            .unwrap()
            .is_empty());
    }
}

#[tokio::test]
async fn strict_ask_requests_consent_for_each_workspace_file_change() {
    let fixture = Fixture::new(Some(workspace::Mode::Ask));
    let path = fixture.key.path.join("nested/file.txt");
    let approve = async {
        let question = fixture.question().await;
        assert!(!path.exists());
        approval::decide(fixture.home.path(), &question.id, Answer::Once).unwrap();
    };
    let (result, ()) = tokio::join!(fixture.write(&path), approve);
    result.unwrap();
    let approve = async {
        let question = fixture.question().await;
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello world");
        approval::decide(fixture.home.path(), &question.id, Answer::Once).unwrap();
    };
    let (result, ()) = tokio::join!(fixture.edit(&path, false, None), approve);
    result.unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), "hello reader");
    fixture.assert_clear();
}

#[tokio::test]
async fn new_file_consent_precedes_directory_creation_and_cannot_be_remembered() {
    let fixture = Fixture::new(Some(workspace::Mode::Ask));
    let path = fixture.outside();
    let answer = async {
        let question = fixture.question().await;
        assert!(!path.parent().unwrap().exists());
        assert_eq!(question.filesystem().unwrap().path, path);
        assert_eq!(question.filesystem().unwrap().label, "Write file");
        assert_eq!(question.answers(), vec![Answer::Once, Answer::Deny]);
        assert!(approval::decide(fixture.home.path(), &question.id, Answer::AlwaysFolder).is_err());
        assert!(approval::decide(fixture.home.path(), &question.id, Answer::AlwaysPath).is_err());
        approval::decide(fixture.home.path(), &question.id, Answer::Once).unwrap();
    };
    let (result, ()) = tokio::join!(fixture.write(&path), answer);
    result.unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello world");
    assert!(workspace::get(fixture.home.path(), &fixture.key)
        .unwrap()
        .unwrap()
        .dirs
        .is_empty());
    fixture.assert_clear();
}

#[tokio::test]
async fn denied_native_file_changes_have_no_effects() {
    for edit in [false, true] {
        let fixture = Fixture::new(Some(workspace::Mode::Ask));
        let path = fixture.outside();
        if edit {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "hello world").unwrap();
        }
        let call = async {
            if edit {
                fixture.edit(&path, false, None).await
            } else {
                fixture.write(&path).await
            }
        };
        let answer = async {
            let question = fixture.question().await;
            approval::decide(fixture.home.path(), &question.id, Answer::Deny).unwrap();
        };
        let (result, ()) = tokio::join!(call, answer);
        assert!(matches!(
            &result,
            Err(ToolError::ReportedFailure {
                class: crate::tool_call_lifecycle::FailureClass::PolicyDenied,
                ..
            })
        ));
        assert!(result.unwrap_err().to_string().contains("not approved"));
        if edit {
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello world");
        } else {
            assert!(!path.parent().unwrap().exists());
        }
        fixture.assert_clear();
    }
}

#[tokio::test]
async fn edit_consent_preserves_the_stale_content_guard() {
    let fixture = Fixture::new(Some(workspace::Mode::Ask));
    let path = fixture.outside();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "hello world").unwrap();
    let answer = async {
        let question = fixture.question().await;
        std::fs::write(&path, "changed by another editor").unwrap();
        approval::decide(fixture.home.path(), &question.id, Answer::Once).unwrap();
    };
    let (result, ()) = tokio::join!(
        fixture.edit(&path, false, Some(content_hash(b"hello world"))),
        answer
    );
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("changed since it was read"));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "changed by another editor"
    );
    fixture.assert_clear();
}

#[tokio::test]
async fn file_consent_is_rechecked_after_waiting_for_the_mutation_lock() {
    let fixture = Fixture::new(Some(workspace::Mode::Ask));
    let path = fixture.outside();
    let lock = file_mutation_lock_for(&path);
    let guard = lock.lock().await;
    let answer = async {
        let question = fixture.question().await;
        approval::decide(fixture.home.path(), &question.id, Answer::Once).unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while !approval::pending_requests(fixture.home.path())
                .unwrap()
                .is_empty()
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        workspace::save_mode(
            fixture.home.path(),
            &fixture.key,
            1,
            workspace::Mode::FullAccess,
            None,
        )
        .unwrap();
        drop(guard);
    };
    let (result, ()) = tokio::join!(fixture.write(&path), answer);
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("permissions changed"));
    assert!(!path.parent().unwrap().exists());
    fixture.assert_clear();
}

#[tokio::test]
async fn newly_created_target_cannot_consume_an_earlier_creation_approval() {
    let fixture = Fixture::new(Some(workspace::Mode::Ask));
    let path = fixture.outside();
    let answer = async {
        let question = fixture.question().await;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "another editor created this").unwrap();
        assert!(approval::decide(fixture.home.path(), &question.id, Answer::Once).is_err());
    };
    let (result, ()) = tokio::join!(fixture.write(&path), answer);
    assert!(result.unwrap_err().to_string().contains("target changed"));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "another editor created this"
    );
    fixture.assert_clear();
}

#[tokio::test]
async fn cancelling_a_file_wait_removes_the_question_without_creating_directories() {
    let fixture = Fixture::new(Some(workspace::Mode::Ask));
    let path = fixture.outside();
    tokio::select! {
        biased;
        _ = fixture.question() => {}
        result = fixture.write(&path) => panic!("unapproved write finished: {result:?}"),
    }
    assert!(!path.parent().unwrap().exists());
    fixture.assert_clear();
}

#[tokio::test]
async fn previews_and_callers_without_preferences_keep_their_existing_access() {
    let fixture = Fixture::new(None);
    let path = fixture.outside();
    fixture.write(&path).await.unwrap();
    workspace::save_mode(
        fixture.home.path(),
        &fixture.key,
        0,
        workspace::Mode::Ask,
        None,
    )
    .unwrap();
    fixture.edit(&path, true, None).await.unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello world");
    fixture.assert_clear();
}

#[tokio::test]
async fn full_access_file_changes_still_obey_the_configured_tool_root() {
    let fixture = Fixture::new(Some(workspace::Mode::FullAccess));
    let path = fixture.outside();
    fixture.write(&path).await.unwrap();
    fixture.edit(&path, false, None).await.unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello reader");
    let outside = tempfile::tempdir().unwrap();
    let excluded = outside.path().join("file.txt");
    assert!(fixture.write(&excluded).await.is_err());
    assert!(!excluded.exists());
    fixture.assert_clear();
}

#[tokio::test]
async fn full_access_cannot_widen_read_only_workspace_authority() {
    use crate::tool_call_lifecycle::runtime::{
        scope_request_tool_execution_with_workspace_overlay, ToolWorkspaceScope,
    };
    let fixture = Fixture::new(Some(workspace::Mode::FullAccess));
    let path = fixture.outside();
    let result = scope_request_tool_execution_with_workspace_overlay(
        None,
        tokio_util::sync::CancellationToken::new(),
        ToolWorkspaceScope {
            runtime_home: None,
            workspace_cwd: Some(fixture.key.path.clone()),
            workspace_root: Some(fixture.root.path().canonicalize().unwrap()),
            workspace_authority: Some(WorkspaceAuthority::ReadOnly),
            workspace_artifact: None,
        },
        None,
        None,
        None,
        Default::default(),
        false,
        fixture.write(&path),
    )
    .await;
    assert!(matches!(
        result,
        Err(ToolError::ReportedFailure {
            class: crate::tool_call_lifecycle::FailureClass::PolicyDenied,
            ..
        })
    ));
    assert!(!path.parent().unwrap().exists());
    fixture.assert_clear();
}

#[cfg(unix)]
#[tokio::test]
async fn replacing_a_pending_creation_parent_with_a_symlink_invalidates_consent() {
    let fixture = Fixture::new(Some(workspace::Mode::Ask));
    let path = fixture.outside();
    let parent = path.parent().unwrap();
    std::fs::create_dir_all(parent).unwrap();
    let answer = async {
        let question = fixture.question().await;
        std::fs::rename(parent, parent.with_extension("preserved")).unwrap();
        std::os::unix::fs::symlink(&fixture.key.path, parent).unwrap();
        assert!(approval::decide(fixture.home.path(), &question.id, Answer::Once).is_err());
    };
    let (result, ()) = tokio::join!(fixture.write(&path), answer);
    assert!(result.unwrap_err().to_string().contains("target changed"));
    assert!(!fixture.key.path.join("file.txt").exists());
    fixture.assert_clear();
}

#[tokio::test]
async fn operator_file_aliases_cannot_be_read_or_changed_with_full_access() {
    use crate::toolset::args::ReadFileArgs;
    let fixture = Fixture::new(Some(workspace::Mode::FullAccess));
    crate::plugin::allowed::protect(fixture.home.path());
    let settings = workspace::preferences_file(fixture.home.path());
    let original = std::fs::read(&settings).unwrap();
    let alias = fixture.key.path.join("project-settings.json");
    std::fs::hard_link(&settings, &alias).unwrap();
    let context = ToolContext::new(fixture.root.path().to_owned(), false).unwrap();
    let read = fixture
        .run(Tool::call(
            &ReadFileTool::new(context.clone(), 1024),
            ReadFileArgs {
                path: alias.to_str().unwrap().into(),
                start_line: None,
                end_line: None,
                max_chars: None,
                raw_json: true,
            },
        ))
        .await;
    assert!(read.unwrap_err().to_string().contains("operator"));
    let write = fixture
        .run(Tool::call(
            &WriteFileTool::new(context.clone()),
            WriteFileArgs {
                path: alias.to_str().unwrap().into(),
                content: "replaced".into(),
                expected_content_hash: None,
                overwrite: true,
                raw_json: true,
            },
        ))
        .await;
    assert!(write.unwrap_err().to_string().contains("operator"));
    let edit = fixture
        .run(Tool::call(
            &EditFileTool::new(context),
            EditFileArgs {
                path: alias.to_str().unwrap().into(),
                old_text: String::from_utf8(original.clone()).unwrap(),
                new_text: "replaced".into(),
                replace_all: false,
                raw_json: true,
                dry_run: false,
                match_mode: None,
                operation: None,
                expected_content_hash: None,
            },
        ))
        .await;
    assert!(edit.unwrap_err().to_string().contains("operator"));
    assert_eq!(std::fs::read(&settings).unwrap(), original);
    assert_eq!(std::fs::read(&alias).unwrap(), original);
    fixture.assert_clear();
}

#[tokio::test]
async fn ordinary_project_hard_links_remain_readable_and_editable() {
    use crate::toolset::args::ReadFileArgs;
    let fixture = Fixture::new(Some(workspace::Mode::FullAccess));
    crate::plugin::allowed::protect(fixture.home.path());
    let path = fixture.key.path.join("source.txt");
    let alias = fixture.key.path.join("alias.txt");
    std::fs::write(&path, "hello world").unwrap();
    std::fs::hard_link(&path, &alias).unwrap();
    let context = ToolContext::new(fixture.root.path().to_owned(), false).unwrap();
    let read = fixture
        .run(Tool::call(
            &ReadFileTool::new(context.clone(), 1024),
            ReadFileArgs {
                path: alias.to_str().unwrap().into(),
                start_line: None,
                end_line: None,
                max_chars: None,
                raw_json: true,
            },
        ))
        .await
        .unwrap();
    assert!(read.contains("hello world"));
    fixture.edit(&alias, false, None).await.unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello reader");
    fixture
        .run(Tool::call(
            &WriteFileTool::new(context),
            WriteFileArgs {
                path: alias.to_str().unwrap().into(),
                content: "updated".into(),
                expected_content_hash: None,
                overwrite: true,
                raw_json: true,
            },
        ))
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "updated");
    assert_eq!(std::fs::read_to_string(&alias).unwrap(), "updated");
    fixture.assert_clear();
}

#[cfg(unix)]
#[tokio::test]
async fn write_only_project_files_remain_overwritable() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new(Some(workspace::Mode::FullAccess));
    crate::plugin::allowed::protect(fixture.home.path());
    let path = fixture.key.path.join("write-only.txt");
    std::fs::write(&path, "original").unwrap();
    let original = std::fs::metadata(&path).unwrap().permissions();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o200)).unwrap();
    let context = ToolContext::new(fixture.root.path().to_owned(), false).unwrap();
    let result = fixture
        .run(Tool::call(
            &WriteFileTool::new(context),
            WriteFileArgs {
                path: path.to_str().unwrap().into(),
                content: "updated".into(),
                expected_content_hash: None,
                overwrite: true,
                raw_json: true,
            },
        ))
        .await;
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o200
    );
    std::fs::set_permissions(&path, original).unwrap();
    result.unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "updated");
    fixture.assert_clear();
}
