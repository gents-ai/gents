use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gents::pack::BindAccess;
use gents::plugin::approval::{self, Answer, Request};
use notify::event::{AccessKind, CreateKind, Flag, ModifyKind, RenameMode};
use notify::{Event, EventKind};
use tokio::sync::{mpsc, watch};

use super::{changes_queue, watch_queue, PendingQueue, PromptSubscriptions, QueueWatch};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn question(id: &str) -> Request {
    Request {
        id: id.to_owned(),
        plugin: "team/ocr".to_owned(),
        path: "/data/x.pdf".to_owned(),
        folder: PathBuf::from("/data"),
        is_dir: false,
        access: BindAccess::Read,
        session_id: None,
        created_at_ms: now_ms(),
        expires_at_ms: now_ms() + approval::WAIT.as_millis() as u64,
    }
}

fn file_question(home: &Path, id: &str) {
    use std::io::Write;
    let queue = approval::queue_dir(home);
    std::fs::create_dir_all(&queue).unwrap();
    let mut file = tempfile::NamedTempFile::new_in(&queue).unwrap();
    file.write_all(&serde_json::to_vec(&question(id)).unwrap())
        .unwrap();
    file.persist(queue.join(format!("{id}.json"))).unwrap();
}

async fn signalled<T>(receive: &mut mpsc::Receiver<T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), receive.recv())
        .await
        .expect("native queue change was not delivered")
        .expect("watch ended")
}

#[test]
fn pending_changes_include_questions_and_answers() {
    let home = tempfile::tempdir().unwrap();
    let mut pending = PendingQueue::default();
    assert!(!pending.refresh(home.path()).unwrap());
    assert!(pending.expires_at.is_none());
    file_question(home.path(), "q1");
    assert!(pending.refresh(home.path()).unwrap());
    assert!(pending.expires_at.is_some());
    assert!(!pending.refresh(home.path()).unwrap());
    approval::decide(home.path(), "q1", Answer::Once).unwrap();
    assert!(pending.refresh(home.path()).unwrap());
    assert!(pending.expires_at.is_none());
}

#[test]
fn listener_marks_and_read_notifications_are_not_queue_changes() {
    let queue = Path::new("/home/plugin-approvals");
    let create = |name| Event::new(EventKind::Create(CreateKind::File)).add_path(queue.join(name));
    assert!(!changes_queue(&create(".listening"), queue));
    assert!(changes_queue(&create("temporary"), queue));
    assert!(changes_queue(&create("q1.json"), queue));
    assert!(changes_queue(&create("q1.decision"), queue));
    assert!(!changes_queue(
        &Event::new(EventKind::Access(AccessKind::Read)).add_path(queue.join("q1.json")),
        queue
    ));
    assert!(changes_queue(
        &Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
            .add_path(queue.join("temporary"))
            .add_path(queue.join("q1.json")),
        queue
    ));
    assert!(changes_queue(
        &Event::new(EventKind::Any).set_flag(Flag::Rescan),
        queue
    ));
}

#[tokio::test]
async fn filesystem_notifications_cover_atomic_questions_and_decisions() {
    let home = tempfile::tempdir().unwrap();
    let (send, mut receive) = mpsc::channel(1);
    let _watch = QueueWatch::start(home.path(), send).unwrap();
    let mut pending = PendingQueue::default();
    pending.refresh(home.path()).unwrap();
    file_question(home.path(), "q1");
    loop {
        signalled(&mut receive).await;
        if pending.refresh(home.path()).unwrap() {
            break;
        }
    }
    while receive.try_recv().is_ok() {}
    approval::decide(home.path(), "q1", Answer::Once).unwrap();
    loop {
        signalled(&mut receive).await;
        if pending.refresh(home.path()).unwrap() {
            break;
        }
    }
    assert!(pending.seen.is_empty());
}

#[tokio::test]
async fn a_partial_question_is_announced_when_its_write_finishes() {
    let home = tempfile::tempdir().unwrap();
    let (send, mut receive) = mpsc::channel(1);
    let mut watch = QueueWatch::start(home.path(), send).unwrap();
    let mut pending = PendingQueue::default();
    let path = approval::queue_dir(home.path()).join("q1.json");
    std::fs::write(&path, b"{").unwrap();
    signalled(&mut receive).await;
    watch.reattach(home.path()).unwrap();
    assert!(!pending.refresh(home.path()).unwrap());
    std::fs::write(&path, serde_json::to_vec(&question("q1")).unwrap()).unwrap();
    loop {
        signalled(&mut receive).await;
        if pending.refresh(home.path()).unwrap() {
            break;
        }
    }
    assert_eq!(pending.seen.into_iter().collect::<Vec<_>>(), ["q1"]);
}

#[tokio::test]
async fn native_watch_recovers_after_the_queue_is_replaced() {
    let home = tempfile::tempdir().unwrap();
    let (send, mut receive) = mpsc::channel(8);
    let (_active, subscriptions) = watch::channel(true);
    let task = tokio::spawn(watch_queue(
        home.path().to_owned(),
        Duration::from_millis(100),
        subscriptions,
        move |changed| {
            if changed {
                let _ = send.try_send(());
            }
        },
    ));
    file_question(home.path(), "q1");
    signalled(&mut receive).await;
    std::fs::remove_dir_all(approval::queue_dir(home.path())).unwrap();
    signalled(&mut receive).await;
    assert!(approval::pending_requests(home.path()).unwrap().is_empty());
    assert!(
        approval::queue_dir(home.path())
            .join(".listening")
            .metadata()
            .unwrap()
            .modified()
            .unwrap()
            > UNIX_EPOCH,
        "reattaching a live prompt must renew the replacement queue immediately"
    );
    file_question(home.path(), "q2");
    signalled(&mut receive).await;
    let pending = approval::pending_requests(home.path()).unwrap();
    assert_eq!(
        pending
            .iter()
            .map(|request| request.id.as_str())
            .collect::<Vec<_>>(),
        ["q2"]
    );
    task.abort();
}

#[tokio::test]
async fn native_heartbeat_keeps_listening_without_queue_reads_or_webview_commands() {
    let home = tempfile::tempdir().unwrap();
    let (send, mut receive) = mpsc::channel(8);
    let (_active, subscriptions) = watch::channel(true);
    let task = tokio::spawn(watch_queue(
        home.path().to_owned(),
        Duration::from_millis(100),
        subscriptions,
        move |changed| {
            let _ = send.try_send(changed);
        },
    ));
    let mark = approval::queue_dir(home.path()).join(".listening");
    assert!(!signalled(&mut receive).await, "the startup queue is empty");
    let stale = SystemTime::now() - Duration::from_secs(60);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&mark)
        .unwrap()
        .set_modified(stale)
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while mark.metadata().unwrap().modified().unwrap() <= stale {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the Rust heartbeat did not renew the listener mark");
    assert!(
        receive.try_recv().is_err(),
        "heartbeats must not scan the queue"
    );
    task.abort();
}

#[tokio::test]
async fn a_replaced_queue_root_reattaches_the_current_format_watch() {
    let home = tempfile::tempdir().unwrap();
    let (send, mut receive) = mpsc::channel(8);
    let (_active, subscriptions) = watch::channel(true);
    let task = tokio::spawn(watch_queue(
        home.path().to_owned(),
        Duration::from_millis(100),
        subscriptions,
        move |changed| {
            if changed {
                let _ = send.try_send(());
            }
        },
    ));
    file_question(home.path(), "first");
    signalled(&mut receive).await;
    std::fs::remove_dir_all(approval::queue_dir(home.path()).parent().unwrap()).unwrap();
    signalled(&mut receive).await;
    assert!(approval::pending_requests(home.path()).unwrap().is_empty());
    file_question(home.path(), "second");
    signalled(&mut receive).await;
    assert_eq!(
        approval::pending_requests(home.path()).unwrap()[0].id,
        "second"
    );
    task.abort();
}

#[tokio::test]
async fn a_short_waiter_deadline_expires_without_waiting_for_the_heartbeat() {
    let home = tempfile::tempdir().unwrap();
    let queue = approval::prepare_listener(home.path()).unwrap();
    let (send, mut receive) = mpsc::channel(8);
    let (_active, subscriptions) = watch::channel(true);
    let task = tokio::spawn(watch_queue(
        home.path().to_owned(),
        Duration::from_secs(60),
        subscriptions,
        move |changed| {
            let _ = send.try_send(changed);
        },
    ));
    assert!(!signalled(&mut receive).await);
    let mut request = question("short");
    request.expires_at_ms = now_ms() + 2_000;
    std::fs::write(
        queue.join("short.json"),
        serde_json::to_vec(&request).unwrap(),
    )
    .unwrap();
    while !signalled(&mut receive).await {}
    assert_eq!(approval::pending_requests(home.path()).unwrap().len(), 1);
    while !signalled(&mut receive).await {}
    assert!(approval::pending_requests(home.path()).unwrap().is_empty());
    task.abort();
}

#[test]
fn prompt_tokens_keep_other_mounts_and_windows_subscribed() {
    let mut subscriptions = PromptSubscriptions::default();
    let active = subscriptions.changes();
    assert!(!*active.borrow());
    subscriptions.subscribe("main", "old".into());
    subscriptions.subscribe("main", "new".into());
    subscriptions.unsubscribe("main", "old");
    subscriptions.unsubscribe("second", "new");
    assert!(subscriptions.contains_window("main"));
    assert!(*active.borrow());
    subscriptions.subscribe("second", "second".into());
    subscriptions.remove_window("main");
    assert!(*active.borrow());
    subscriptions.unsubscribe("second", "second");
    assert!(!*active.borrow());
}

#[tokio::test]
async fn native_watch_advertises_only_while_a_prompt_is_subscribed() {
    let home = tempfile::tempdir().unwrap();
    let (send, mut receive) = mpsc::channel(8);
    let (active, subscriptions) = watch::channel(false);
    let task = tokio::spawn(watch_queue(
        home.path().to_owned(),
        Duration::from_millis(20),
        subscriptions,
        move |changed| {
            if changed {
                let _ = send.try_send(changed);
            }
        },
    ));
    let mark = approval::queue_dir(home.path()).join(".listening");
    file_question(home.path(), "q1");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !mark.exists(),
        "startup without a prompt must not advertise"
    );
    assert!(receive.try_recv().is_err());

    active.send_replace(true);
    assert!(signalled(&mut receive).await);
    assert!(mark.metadata().is_ok());
    active.send_replace(false);
    let stale = SystemTime::now() - Duration::from_secs(60);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&mark)
        .unwrap()
        .set_modified(stale)
        .unwrap();
    file_question(home.path(), "q2");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(mark.metadata().unwrap().modified().unwrap(), stale);
    assert!(receive.try_recv().is_err());

    approval::mark_listening(home.path()).unwrap();
    let cli_mark = mark.metadata().unwrap().modified().unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        mark.metadata().unwrap().modified().unwrap(),
        cli_mark,
        "unsubscribing must preserve a CLI listener's mark"
    );
    task.abort();
}

#[tokio::test]
async fn an_unavailable_queue_does_not_advertise_a_prompt_listener() {
    let home = tempfile::tempdir().unwrap();
    let queue = approval::queue_dir(home.path());
    std::fs::create_dir_all(queue.parent().unwrap()).unwrap();
    std::fs::write(&queue, b"not a directory").unwrap();
    let (_active, subscriptions) = watch::channel(true);
    let task = tokio::spawn(watch_queue(
        home.path().to_owned(),
        Duration::from_millis(20),
        subscriptions,
        |_| panic!("an unavailable queue must not be announced"),
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!queue.join(".listening").exists());
    assert!(!task.is_finished(), "the native watcher must stay alive");
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn an_incompatible_question_does_not_advertise_a_prompt_listener() {
    let home = tempfile::tempdir().unwrap();
    let queue = approval::prepare_listener(home.path()).unwrap();
    let mut value = serde_json::to_value(question("q1")).unwrap();
    value.as_object_mut().unwrap().remove("expires_at_ms");
    std::fs::write(queue.join("q1.json"), serde_json::to_vec(&value).unwrap()).unwrap();
    let (_active, subscriptions) = watch::channel(true);
    let task = tokio::spawn(watch_queue(
        home.path().to_owned(),
        Duration::from_millis(20),
        subscriptions,
        |_| panic!("an incompatible queue must not be announced"),
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        queue
            .join(".listening")
            .metadata()
            .unwrap()
            .modified()
            .unwrap(),
        UNIX_EPOCH
    );
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
}

#[cfg(target_os = "macos")]
#[test]
fn unrelated_home_changes_do_not_exhaust_watch_descriptors() {
    use std::os::unix::ffi::OsStrExt;
    const CHILD: &str = "GENTS_APPROVAL_WATCH_FD_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "approvals_watch::tests::unrelated_home_changes_do_not_exhaust_watch_descriptors",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let mut limits = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: this subprocess owns its limits; the initialized rlimit storage is valid.
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limits) },
        0
    );
    limits.rlim_cur = 256.min(limits.rlim_max);
    // SAFETY: only this test subprocess's soft limit changes, within its hard limit.
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limits) }, 0);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let home = tempfile::tempdir().unwrap();
            let unrelated = home.path().join("unrelated-pack");
            std::fs::create_dir(&unrelated).unwrap();
            for index in 0..512 {
                std::fs::write(
                    unrelated.join(format!("schema-{index}.graphql")),
                    "type Example",
                )
                .unwrap();
            }
            let (send, _receive) = mpsc::channel(1);
            let _watch = QueueWatch::start(home.path(), send).unwrap();
            std::fs::create_dir(home.path().join("unrelated-directory-change")).unwrap();
            tokio::time::sleep(Duration::from_millis(500)).await;
            let unrelated = unrelated.canonicalize().unwrap();
            for fd in 0..256 {
                let mut path = [0 as libc::c_char; libc::MAXPATHLEN as usize];
                // SAFETY: F_GETPATH writes at most MAXPATHLEN bytes and succeeds
                // only with a terminated path; failed descriptors are ignored.
                if unsafe { libc::fcntl(fd, libc::F_GETPATH, path.as_mut_ptr()) } == 0 {
                    let path = unsafe { std::ffi::CStr::from_ptr(path.as_ptr()) };
                    let path = Path::new(std::ffi::OsStr::from_bytes(path.to_bytes()));
                    assert!(
                        !path.starts_with(&unrelated),
                        "approval watcher opened unrelated path {}",
                        path.display()
                    );
                }
            }
            let _headroom: Vec<_> = (0..64)
                .map(|_| std::fs::File::open(unrelated.join("schema-0.graphql")).unwrap())
                .collect();
            file_question(home.path(), "stillreadable");
            assert_eq!(approval::pending_requests(home.path()).unwrap().len(), 1);
        });
}
