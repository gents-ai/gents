use super::*;
use gents::plugin::approval::workspace::{self, Mode, Settings, WorkspaceKey};

struct Workspace {
    root: tempfile::TempDir,
    home: PathBuf,
    settings: Settings,
    request: Request,
}

impl Workspace {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let project = root.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let path = project.join("document.txt");
        std::fs::write(&path, b"document").unwrap();
        let path = path.canonicalize().unwrap();
        let key = WorkspaceKey::canonical("node", "requester", "agent", &project).unwrap();
        let settings = workspace::save_mode(&home, &key, 0, Mode::Ask, None).unwrap();
        let request = Request::new(
            "team/files",
            &gents::plugin::allowed::Resolved {
                target: path.clone(),
                is_dir: false,
            },
            BindAccess::ReadWrite,
            Some("session".into()),
        );
        let value = serde_json::json!({
            "id": request.id, "session_id": request.session_id,
            "created_at_ms": request.created_at_ms, "expires_at_ms": request.expires_at_ms,
            "binding": {
                "scope": settings.scope(),
                "action": {
                    "kind": "plugin",
                    "plugin": "team/files",
                    "artifactDigest": "artifact",
                    "inputDigest": "input",
                    "path": path,
                    "isDir": false,
                    "access": BindAccess::ReadWrite,
                },
            },
        });
        Self {
            root,
            home,
            settings,
            request: serde_json::from_value(value).unwrap(),
        }
    }

    fn question_file(&self) -> PathBuf {
        approval::queue_dirs(&self.home)[1].join(format!("{}.json", self.request.id))
    }

    fn file(&self) {
        let path = self.question_file();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let staged = tempfile::NamedTempFile::new_in(path.parent().unwrap()).unwrap();
        serde_json::to_writer(staged.as_file(), &self.request).unwrap();
        staged.persist(path).unwrap();
    }

    fn change_mode(&self) {
        workspace::save_mode(
            &self.home,
            &self.settings.workspace,
            self.settings.selection.revision,
            Mode::FullAccess,
            None,
        )
        .unwrap();
    }

    fn command(&self) -> Request {
        let mut value = serde_json::to_value(&self.request).unwrap();
        value["binding"]["action"] = serde_json::json!({
            "kind": "command", "argv": ["echo", "approved"],
            "cwd": self.settings.workspace.path,
            "root": self.root.path().canonicalize().unwrap(),
            "policy": gents::tools::native::CommandExecutionPolicy::write_capable(),
        });
        serde_json::from_value(value).unwrap()
    }
}

async fn until_pending_changes(
    home: &Path,
    pending: &mut PendingQueue,
    receive: &mut mpsc::Receiver<()>,
) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            signalled(receive).await;
            if pending.refresh(home).unwrap() {
                break;
            }
        }
    })
    .await
    .expect("approval state did not change");
}

#[tokio::test]
async fn changing_workspace_permissions_removes_an_orphaned_prompt_immediately() {
    let fixture = Workspace::new();
    let (send, mut receive) = mpsc::channel(1);
    let _watch = QueueWatch::start(&fixture.home, send).unwrap();
    let mut pending = PendingQueue::default();
    assert!(!pending.refresh(&fixture.home).unwrap());
    fixture.file();
    until_pending_changes(&fixture.home, &mut pending, &mut receive).await;
    assert_eq!(pending.seen.len(), 1);
    while receive.try_recv().is_ok() {}
    fixture.change_mode();
    until_pending_changes(&fixture.home, &mut pending, &mut receive).await;
    assert!(pending.seen.is_empty());
    assert!(approval::decide(&fixture.home, &fixture.request.id, Answer::AlwaysFolder).is_err());
    assert!(gents::plugin::allowed::list(&fixture.home)
        .unwrap()
        .is_empty());
    assert!(workspace::get(&fixture.home, &fixture.settings.workspace)
        .unwrap()
        .unwrap()
        .dirs
        .is_empty());
}

#[tokio::test]
async fn atomic_permission_replacements_remain_observable_without_queue_activity() {
    async fn wait_revision(observed: &mut watch::Receiver<u64>, expected: u64) {
        drop(
            tokio::time::timeout(
                Duration::from_secs(5),
                observed.wait_for(|revision| *revision == expected),
            )
            .await
            .expect("permission revision was not observed")
            .expect("permission observer ended"),
        );
    }
    let fixture = Workspace::new();
    let home = fixture.home.clone();
    let key = fixture.settings.workspace.clone();
    let (subscriptions, subscribed) = watch::channel(true);
    let (changes, mut observed) = watch::channel(0);
    let observing_home = home.clone();
    let observing_key = key.clone();
    let task = tokio::spawn(watch_queue(
        home.clone(),
        Duration::from_secs(3_600),
        subscribed,
        move |_| {
            let revision = workspace::get(&observing_home, &observing_key)
                .unwrap()
                .unwrap()
                .selection
                .revision;
            changes.send_replace(revision);
        },
    ));
    let mut revision = fixture.settings.selection.revision;
    wait_revision(&mut observed, revision).await;
    for mode in [Mode::FullAccess, Mode::Ask, Mode::FullAccess] {
        revision = workspace::save_mode(&home, &key, revision, mode, None)
            .unwrap()
            .selection
            .revision;
        wait_revision(&mut observed, revision).await;
    }
    drop(subscriptions);
    task.await.unwrap();
}

#[tokio::test]
async fn remembered_workspace_access_reaches_the_waiter_without_widening_global_access() {
    let fixture = Workspace::new();
    approval::mark_listening(&fixture.home).unwrap();
    let (send, mut receive) = mpsc::channel(1);
    let _watch = QueueWatch::start(&fixture.home, send).unwrap();
    let mut pending = PendingQueue::default();
    let waiting = approval::ask(&fixture.home, &fixture.request, Duration::from_secs(10));
    let answer = async {
        until_pending_changes(&fixture.home, &mut pending, &mut receive).await;
        approval::decide(&fixture.home, &fixture.request.id, Answer::AlwaysFolder).unwrap();
    };
    let (allowed, ()) = tokio::join!(waiting, answer);
    assert!(allowed.unwrap());
    assert!(!fixture.question_file().exists());
    assert!(gents::plugin::allowed::list(&fixture.home)
        .unwrap()
        .is_empty());
    let settings = workspace::get(&fixture.home, &fixture.settings.workspace)
        .unwrap()
        .unwrap();
    assert_eq!(settings.dirs.len(), 1);
    assert_eq!(
        settings.dirs[0].path.as_path(),
        fixture.request.filesystem().unwrap().folder
    );
    assert_eq!(settings.dirs[0].access, BindAccess::ReadWrite);
    let other = fixture.root.path().join("other");
    std::fs::create_dir_all(&other).unwrap();
    let other = WorkspaceKey::canonical("node", "requester", "agent", &other).unwrap();
    assert!(workspace::get(&fixture.home, &other).unwrap().is_none());
}

#[tokio::test]
async fn changing_permissions_cancels_the_waiting_question() {
    let fixture = Workspace::new();
    let (send, mut receive) = mpsc::channel(1);
    let _watch = QueueWatch::start(&fixture.home, send).unwrap();
    let mut pending = PendingQueue::default();
    let waiting = approval::ask(&fixture.home, &fixture.request, Duration::from_secs(10));
    let change = async {
        until_pending_changes(&fixture.home, &mut pending, &mut receive).await;
        fixture.change_mode();
    };
    let (result, ()) = tokio::join!(waiting, change);
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("Workspace permissions changed"));
    assert!(!fixture.question_file().exists());
}

#[tokio::test]
async fn command_questions_resume_once_without_granting_filesystem_access() {
    use crate::plugin_access::{ApprovalSubjectView, PluginApprovalView};
    let fixture = Workspace::new();
    let request = fixture.command();
    approval::mark_listening(&fixture.home).unwrap();
    let (send, mut receive) = mpsc::channel(1);
    let _watch = QueueWatch::start(&fixture.home, send).unwrap();
    let mut pending = PendingQueue::default();
    let waiting = approval::ask(&fixture.home, &request, Duration::from_secs(10));
    let answer = async {
        until_pending_changes(&fixture.home, &mut pending, &mut receive).await;
        let questions = approval::pending_requests(&fixture.home).unwrap();
        let view = PluginApprovalView::from(&questions[0]);
        assert!(matches!(view.subject, ApprovalSubjectView::Command { .. }));
        assert_eq!(view.answers, [Answer::Once, Answer::Deny]);
        assert!(approval::decide(&fixture.home, &request.id, Answer::AlwaysFolder).is_err());
        assert_eq!(approval::pending_requests(&fixture.home).unwrap().len(), 1);
        approval::decide(&fixture.home, &request.id, Answer::Once).unwrap();
    };
    let (allowed, ()) = tokio::join!(waiting, answer);
    assert!(allowed.unwrap());
    assert!(!fixture.question_file().exists());
    assert!(gents::plugin::allowed::list(&fixture.home)
        .unwrap()
        .is_empty());
    assert!(workspace::get(&fixture.home, &fixture.settings.workspace)
        .unwrap()
        .unwrap()
        .dirs
        .is_empty());
}

#[tokio::test]
async fn an_unscoped_listener_cannot_keep_a_workspace_question_waiting() {
    let fixture = Workspace::new();
    approval::prepare_listener(&fixture.home).unwrap();
    let mark = approval::queue_dir(&fixture.home).join(".listening");
    std::fs::File::options()
        .write(true)
        .open(mark)
        .unwrap()
        .set_modified(SystemTime::now())
        .unwrap();
    let error = approval::ask(&fixture.home, &fixture.request, Duration::from_secs(10))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("updated client"));
    assert!(!fixture.question_file().exists());
    assert!(gents::plugin::allowed::list(&fixture.home)
        .unwrap()
        .is_empty());
}

#[test]
fn duplicate_question_ids_across_scopes_cannot_be_answered() {
    let fixture = Workspace::new();
    fixture.file();
    file_question(&fixture.home, &fixture.request.id);
    assert!(approval::pending_requests(&fixture.home).is_err());
    assert!(approval::decide(&fixture.home, &fixture.request.id, Answer::AlwaysFolder).is_err());
    assert!(gents::plugin::allowed::list(&fixture.home)
        .unwrap()
        .is_empty());
}

#[test]
fn workspace_questions_cannot_be_answered_through_the_unscoped_queue() {
    let fixture = Workspace::new();
    fixture.file();
    let unscoped = approval::queue_dir(&fixture.home);
    std::fs::create_dir_all(&unscoped).unwrap();
    std::fs::rename(
        fixture.question_file(),
        unscoped.join(format!("{}.json", fixture.request.id)),
    )
    .unwrap();
    assert!(approval::pending_requests(&fixture.home).is_err());
    assert!(approval::decide(&fixture.home, &fixture.request.id, Answer::AlwaysFolder).is_err());
    assert!(gents::plugin::allowed::list(&fixture.home)
        .unwrap()
        .is_empty());
}

#[cfg(unix)]
#[test]
fn a_changed_file_cannot_consume_an_approval_for_the_original_path() {
    let fixture = Workspace::new();
    fixture.file();
    let other = fixture.root.path().join("other.txt");
    std::fs::write(&other, b"other document").unwrap();
    std::fs::remove_file(fixture.request.filesystem().unwrap().path).unwrap();
    std::os::unix::fs::symlink(&other, fixture.request.filesystem().unwrap().path).unwrap();
    assert!(approval::decide(&fixture.home, &fixture.request.id, Answer::Once).is_err());
    file_question(&fixture.home, "unaffected");
    let pending = approval::pending_requests(&fixture.home).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, "unaffected");
}

#[cfg(unix)]
#[test]
fn a_changed_folder_cannot_receive_a_remembered_workspace_grant() {
    let fixture = Workspace::new();
    fixture.file();
    let other = fixture.root.path().join("other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("document.txt"), b"other document").unwrap();
    std::fs::rename(
        fixture.request.filesystem().unwrap().folder,
        fixture.root.path().join("original"),
    )
    .unwrap();
    std::os::unix::fs::symlink(&other, fixture.request.filesystem().unwrap().folder).unwrap();
    assert!(approval::decide(&fixture.home, &fixture.request.id, Answer::AlwaysFolder).is_err());
    assert!(workspace::get(&fixture.home, &fixture.settings.workspace)
        .unwrap()
        .unwrap()
        .dirs
        .is_empty());
    assert!(gents::plugin::allowed::list(&fixture.home)
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn a_changed_path_retires_its_waiting_question() {
    let fixture = Workspace::new();
    let (send, mut receive) = mpsc::channel(1);
    let _watch = QueueWatch::start(&fixture.home, send).unwrap();
    let mut pending = PendingQueue::default();
    let waiting = approval::ask(&fixture.home, &fixture.request, Duration::from_secs(10));
    let remove = async {
        until_pending_changes(&fixture.home, &mut pending, &mut receive).await;
        std::fs::remove_file(fixture.request.filesystem().unwrap().path).unwrap();
    };
    let (result, ()) = tokio::join!(waiting, remove);
    assert!(result.unwrap_err().to_string().contains("path changed"));
    assert!(!fixture.question_file().exists());
}

#[test]
fn a_file_replaced_by_a_directory_cannot_receive_the_file_approval() {
    let fixture = Workspace::new();
    fixture.file();
    std::fs::remove_file(fixture.request.filesystem().unwrap().path).unwrap();
    std::fs::create_dir(fixture.request.filesystem().unwrap().path).unwrap();
    assert!(approval::decide(&fixture.home, &fixture.request.id, Answer::AlwaysPath).is_err());
    assert!(workspace::get(&fixture.home, &fixture.settings.workspace)
        .unwrap()
        .unwrap()
        .dirs
        .is_empty());
}
