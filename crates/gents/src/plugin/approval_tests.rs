use super::*;
use std::time::Duration;

fn request(home: &Path) -> Request {
    let folder = home.join("docs");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("x.pdf"), "pdf").unwrap();
    let resolved = allowed::Resolved {
        target: folder.canonicalize().unwrap().join("x.pdf"),
        is_dir: false,
    };
    Request::new(
        "team/ocr",
        &resolved,
        BindAccess::Read,
        Some("session-1".into()),
    )
}

#[test]
fn the_prompt_names_the_plugin_the_exact_path_and_the_access() {
    let home = tempfile::tempdir().unwrap();
    let request = request(home.path());
    assert!(request.prompt().starts_with("Allow team/ocr to read "));
    assert!(request.prompt().ends_with("x.pdf?"));
    let file = request.filesystem().unwrap();
    let write = Request::new(
        file.label,
        &allowed::Resolved {
            target: file.path.to_owned(),
            is_dir: file.is_dir,
        },
        BindAccess::ReadWrite,
        request.session_id.clone(),
    );
    assert!(write.prompt().contains("to read and write"));
}

#[test]
fn preparing_a_watch_does_not_advertise_a_listener_or_stale_an_existing_one() {
    let home = tempfile::tempdir().unwrap();
    prepare_listener(home.path()).unwrap();
    assert!(!listener_fresh(home.path(), Queue::Unscoped));
    mark_listening(home.path()).unwrap();
    let before = std::fs::metadata(Queue::Unscoped.listener_file(home.path()))
        .unwrap()
        .modified()
        .unwrap();
    prepare_listener(home.path()).unwrap();
    assert!(listener_fresh(home.path(), Queue::Unscoped));
    assert_eq!(
        std::fs::metadata(Queue::Unscoped.listener_file(home.path()))
            .unwrap()
            .modified()
            .unwrap(),
        before
    );
}

#[test]
fn queue_reads_do_not_advertise_or_refresh_a_listener() {
    let home = tempfile::tempdir().unwrap();
    assert!(pending_requests(home.path()).unwrap().is_empty());
    assert!(!queue_dir(home.path()).exists());

    let request = request(home.path());
    write_atomic(
        &Queue::Unscoped.request_file(home.path(), &request.id),
        &serde_json::to_vec(&request).unwrap(),
    )
    .unwrap();
    assert_eq!(
        pending_requests(home.path()).unwrap(),
        std::slice::from_ref(&request)
    );
    assert!(!Queue::Unscoped.listener_file(home.path()).exists());

    assert_eq!(
        pending(home.path()).unwrap(),
        std::slice::from_ref(&request)
    );
    assert!(listener_fresh(home.path(), Queue::Unscoped));
    let mark = Queue::Unscoped.listener_file(home.path());
    let fresh = mark.metadata().unwrap().modified().unwrap();
    pending_requests(home.path()).unwrap();
    assert_eq!(mark.metadata().unwrap().modified().unwrap(), fresh);

    std::fs::OpenOptions::new()
        .write(true)
        .open(&mark)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH)
        .unwrap();
    assert_eq!(
        pending_requests(home.path()).unwrap(),
        std::slice::from_ref(&request)
    );
    assert!(!listener_fresh(home.path(), Queue::Unscoped));
    assert_eq!(pending(home.path()).unwrap(), [request]);
    assert!(listener_fresh(home.path(), Queue::Unscoped));
}

#[tokio::test]
async fn an_allow_reaches_the_waiting_call_and_clears_the_question() {
    let home = tempfile::tempdir().unwrap();
    let request = request(home.path());
    let waiting = {
        let home = home.path().to_owned();
        let request = request.clone();
        tokio::spawn(async move { ask(&home, &request, Duration::from_secs(10)).await })
    };
    while pending(home.path()).unwrap().is_empty() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let mut expected = request.clone();
    let listed = pending(home.path()).unwrap();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].expires_at_ms < request.expires_at_ms);
    expected.expires_at_ms = listed[0].expires_at_ms;
    assert_eq!(listed, [expected]);
    decide(home.path(), &request.id, Answer::Once).unwrap();
    assert!(waiting.await.unwrap().unwrap());
    assert!(pending(home.path()).unwrap().is_empty());
    assert!(
        allowed::list(home.path()).unwrap().is_empty(),
        "once remembers nothing"
    );
}

#[tokio::test]
async fn a_deny_reaches_the_waiting_call() {
    let home = tempfile::tempdir().unwrap();
    let request = request(home.path());
    let waiting = {
        let home = home.path().to_owned();
        let request = request.clone();
        tokio::spawn(async move { ask(&home, &request, Duration::from_secs(10)).await })
    };
    while pending(home.path()).unwrap().is_empty() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    decide(home.path(), &request.id, Answer::Deny).unwrap();
    assert!(!waiting.await.unwrap().unwrap());
    assert!(
        allowed::list(home.path()).unwrap().is_empty(),
        "a deny remembers nothing"
    );
}

#[tokio::test]
async fn always_allow_adds_the_folder_and_keeps_wider_access() {
    let home = tempfile::tempdir().unwrap();
    let documents = tempfile::tempdir().unwrap();
    let request = request(documents.path());
    allowed::add(
        home.path(),
        request.filesystem().unwrap().folder,
        BindAccess::ReadWrite,
    )
    .unwrap();
    let waiting = {
        let home = home.path().to_owned();
        let request = request.clone();
        tokio::spawn(async move { ask(&home, &request, Duration::from_secs(10)).await })
    };
    while pending(home.path()).unwrap().is_empty() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    decide(home.path(), &request.id, Answer::AlwaysFolder).unwrap();
    assert!(waiting.await.unwrap().unwrap());
    let listed = allowed::list(home.path()).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].access, BindAccess::ReadWrite);
}

#[tokio::test]
async fn an_unanswered_question_times_out_and_is_removed() {
    let home = tempfile::tempdir().unwrap();
    let request = request(home.path());
    let error = ask(home.path(), &request, Duration::from_millis(60))
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("did not answer"));
    assert!(pending(home.path()).unwrap().is_empty());
}

#[test]
fn only_a_waiting_question_can_be_answered() {
    let home = tempfile::tempdir().unwrap();
    assert!(decide(home.path(), "nope", Answer::Once).is_err());
    assert!(decide(home.path(), "../escape", Answer::Once).is_err());
    assert!(decide(home.path(), "", Answer::Once).is_err());
}

#[test]
fn a_question_cannot_impersonate_another_queue_entry() {
    let home = tempfile::tempdir().unwrap();
    let request = request(home.path());
    write_atomic(
        &Queue::Unscoped.request_file(home.path(), "different"),
        &serde_json::to_vec(&request).unwrap(),
    )
    .unwrap();
    assert!(pending_requests(home.path()).unwrap().is_empty());
    assert!(decide(home.path(), "different", Answer::AlwaysFolder).is_err());
    assert!(allowed::list(home.path()).unwrap().is_empty());
}

#[test]
fn an_expired_question_cannot_publish_an_answer_or_a_persistent_grant() {
    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let mut request = request(outside.path());
    request.expires_at_ms = now_ms().saturating_sub(1);
    request.created_at_ms = request.expires_at_ms.saturating_sub(1);
    write_atomic(
        &Queue::Unscoped.request_file(home.path(), &request.id),
        &serde_json::to_vec(&request).unwrap(),
    )
    .unwrap();
    assert!(pending_requests(home.path()).unwrap().is_empty());
    assert!(decide(home.path(), &request.id, Answer::AlwaysFolder).is_err());
    assert!(allowed::list(home.path()).unwrap().is_empty());
    assert!(!Queue::Unscoped
        .decision_file(home.path(), &request.id)
        .exists());
}

#[test]
fn questions_without_a_waiter_deadline_fail_with_a_version_error() {
    let home = tempfile::tempdir().unwrap();
    let request = request(home.path());
    let mut value = serde_json::to_value(&request).unwrap();
    value.as_object_mut().unwrap().remove("expires_at_ms");
    write_atomic(
        &Queue::Unscoped.request_file(home.path(), &request.id),
        &serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
    let error = pending_requests(home.path()).unwrap_err();
    assert!(format!("{error:#}").contains("matching runtime and client versions"));
    assert!(decide(home.path(), &request.id, Answer::AlwaysFolder).is_err());
    assert!(allowed::list(home.path()).unwrap().is_empty());
}

#[test]
fn incompatible_responders_and_orphaned_questions_do_not_share_the_current_queue() {
    let home = tempfile::tempdir().unwrap();
    let incompatible = home.path().join(crate::home::PLUGIN_APPROVALS_DIR_NAME);
    std::fs::create_dir_all(&incompatible).unwrap();
    let mark = incompatible.join(".listening");
    std::fs::write(&mark, []).unwrap();
    let question = incompatible.join("unanswered.json");
    let bytes = br#"{"id":"unanswered","created_at_ms":0}"#;
    std::fs::write(&question, bytes).unwrap();
    assert!(!listener_fresh(home.path(), Queue::Unscoped));
    assert!(pending_requests(home.path()).unwrap().is_empty());
    mark_listening(home.path()).unwrap();
    assert!(listener_fresh(home.path(), Queue::Unscoped));
    assert_eq!(std::fs::read(question).unwrap(), bytes);
    assert!(decide(home.path(), "unanswered", Answer::AlwaysFolder).is_err());
}

#[tokio::test]
async fn a_cancelled_waiter_cannot_leave_a_grantable_question() {
    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let request = request(outside.path());
    let waiting = {
        let home = home.path().to_owned();
        let request = request.clone();
        tokio::spawn(async move { ask(&home, &request, WAIT).await })
    };
    tokio::time::timeout(Duration::from_secs(2), async {
        while pending(home.path()).unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    waiting.abort();
    assert!(waiting.await.unwrap_err().is_cancelled());
    assert!(decide(home.path(), &request.id, Answer::AlwaysFolder).is_err());
    assert!(allowed::list(home.path()).unwrap().is_empty());
    assert!(pending_requests(home.path()).unwrap().is_empty());
}

#[test]
fn concurrent_answers_deliver_only_one_decision() {
    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let request = request(outside.path());
    write_atomic(
        &Queue::Unscoped.request_file(home.path(), &request.id),
        &serde_json::to_vec(&request).unwrap(),
    )
    .unwrap();
    let barrier = std::sync::Barrier::new(3);
    let results = std::thread::scope(|scope| {
        let once = scope.spawn(|| {
            barrier.wait();
            decide(home.path(), &request.id, Answer::Deny)
        });
        let always = scope.spawn(|| {
            barrier.wait();
            decide(home.path(), &request.id, Answer::AlwaysFolder)
        });
        barrier.wait();
        (once.join().unwrap(), always.join().unwrap())
    });
    assert_ne!(results.0.is_ok(), results.1.is_ok());
    let decision: Decision = serde_json::from_slice(
        &std::fs::read(Queue::Unscoped.decision_file(home.path(), &request.id)).unwrap(),
    )
    .unwrap();
    assert_eq!(decision.allow, results.1.is_ok());
    assert_eq!(
        !allowed::list(home.path()).unwrap().is_empty(),
        results.1.is_ok()
    );
}

#[tokio::test]
async fn always_allow_this_file_adds_only_the_file() {
    let home = tempfile::tempdir().unwrap();
    let documents = tempfile::tempdir().unwrap();
    let request = request(documents.path());
    let waiting = {
        let home = home.path().to_owned();
        let request = request.clone();
        tokio::spawn(async move { ask(&home, &request, Duration::from_secs(10)).await })
    };
    while pending(home.path()).unwrap().is_empty() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    decide(home.path(), &request.id, Answer::AlwaysPath).unwrap();
    assert!(waiting.await.unwrap().unwrap());
    let listed = allowed::list(home.path()).unwrap();
    assert_eq!(listed.len(), 1);
    let file = request.filesystem().unwrap();
    assert_eq!(listed[0].path.as_path(), file.path);
    let scope = allowed::Scope::load(home.path(), None, None).unwrap();
    assert!(scope.granted(file.path).is_some());
    assert!(
        scope.granted(&file.folder.join("other.pdf")).is_none(),
        "the sibling stays closed"
    );
}

#[tokio::test]
async fn a_call_with_nobody_listening_fails_at_once_naming_the_command() {
    let home = tempfile::tempdir().unwrap();
    let request = request(home.path());
    let started = std::time::Instant::now();
    let error = ask(home.path(), &request, Duration::from_secs(60))
        .await
        .unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("gents plugin dirs add"), "{message}");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(pending(home.path()).unwrap().is_empty());
}
