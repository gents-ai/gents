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
    let mut request = request(home.path());
    assert!(request.prompt().starts_with("Allow team/ocr to read "));
    assert!(request.prompt().ends_with("x.pdf?"));
    request.access = BindAccess::ReadWrite;
    assert!(request.prompt().contains("to read and write"));
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
    assert_eq!(pending(home.path()).unwrap(), vec![request.clone()]);
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
    allowed::add(home.path(), &request.folder, BindAccess::ReadWrite).unwrap();
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
    assert_eq!(listed[0].path, PathBuf::from(&request.path));
    let scope = allowed::Scope::load(home.path(), None, None).unwrap();
    assert!(scope.granted(Path::new(&request.path)).is_some());
    assert!(
        scope.granted(&request.folder.join("other.pdf")).is_none(),
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
