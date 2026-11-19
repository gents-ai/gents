use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::plugin::approval;
use approval::workspace::{self, BindingAction, BindingDecision, Mode};

struct Fixture {
    _directory: tempfile::TempDir,
    home: PathBuf,
    key: WorkspaceKey,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("home");
        let project = directory.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let key = WorkspaceKey::canonical("node", "requester", "agent", &project).unwrap();
        workspace::save_mode(
            &home,
            &key,
            0,
            Mode::AutoReview,
            Some(&workspace::ReviewTaskSelection::for_task(
                &gents_documents::Task {
                    task_id: "review".into(),
                    ..workspace::default_review_task("node", "agent")
                },
            )),
        )
        .unwrap();
        Self {
            _directory: directory,
            home,
            key,
        }
    }

    fn context(&self, reviewer: Reviewer) -> Context {
        for_request(
            Some(&self.home),
            "node",
            Some("requester"),
            "agent",
            Some(&self.key.path),
            None,
        )
        .unwrap()
        .unwrap()
        .with_reviewer(reviewer)
    }

    fn decision(&self, context: &Context, covered: bool) -> BindingDecision {
        workspace::binding_route(
            Some(context),
            &self.home,
            || {
                Ok(BindingAction::Command {
                    argv: vec!["echo".into(), "fixture".into()],
                    cwd: self.key.path.clone(),
                    root: self.key.path.clone(),
                    writable_root: None,
                    policy: crate::native::CommandExecutionPolicy::write_capable(),
                })
            },
            covered,
            || Ok(serde_json::json!({"command":"echo", "args":["fixture"]})),
        )
        .unwrap()
        .unwrap()
    }
}

fn approval(context: &Context) -> gents_loop::tool::ToolCallEnvelope {
    crate::runtime_context::ToolHostContext {
        approval: Some(context.clone()),
        ..Default::default()
    }
    .call()
}

fn reply(id: &str, verdict: &str) -> String {
    serde_json::json!({"actionId":id, "decision":verdict, "reason":"Reviewed the requested action."}).to_string()
}

#[tokio::test]
async fn reviews_are_serialized_and_routine_actions_skip_the_reviewer() {
    let fixture = Fixture::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(tokio::sync::Notify::new());
    let observed = Arc::new(Mutex::new(Vec::new()));
    let reviewer: Reviewer = Arc::new({
        let calls = calls.clone();
        let release = release.clone();
        let observed = observed.clone();
        move |invocation| {
            let index = calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(invocation.task.task_id, "review");
            assert_eq!(
                invocation.payload,
                serde_json::json!({"command":"echo", "args":["fixture"]})
            );
            observed.lock().unwrap().push(invocation.action_id.clone());
            let release = release.clone();
            Box::pin(async move {
                if index == 0 {
                    release.notified().await;
                }
                Ok(Some(reply(&invocation.action_id, "allow")))
            })
        }
    });
    let review_context = fixture.context(reviewer);
    async {
        assert!(workspace::consent_binding(
            &approval(&review_context),
            &fixture.home,
            fixture.decision(&review_context, true),
            false,
            None
        )
        .await
        .unwrap());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let ambient_call_117 = approval(&review_context);
        let mut first = Box::pin(workspace::consent_binding(
            &ambient_call_117,
            &fixture.home,
            fixture.decision(&review_context, false),
            false,
            None,
        ));
        let ambient_call_124 = approval(&review_context);
        let mut second = Box::pin(workspace::consent_binding(
            &ambient_call_124,
            &fixture.home,
            fixture.decision(&review_context, false),
            false,
            None,
        ));
        assert!(futures::poll!(first.as_mut()).is_pending());
        assert!(futures::poll!(second.as_mut()).is_pending());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        release.notify_one();
        assert!(first.await.unwrap());
        assert!(second.await.unwrap());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let ids = observed.lock().unwrap();
        assert_ne!(ids[0], ids[1]);
        assert!(approval::pending_requests(&fixture.home)
            .unwrap()
            .is_empty());
    }
    .await;
}

#[tokio::test]
async fn a_routine_decision_cannot_be_carried_to_another_workspace() {
    let fixture = Fixture::new();
    let reviewer: Reviewer = Arc::new(|_| panic!("routine action does not need review"));
    let review_context = fixture.context(reviewer);
    let decision = async { fixture.decision(&review_context, true) }.await;
    let other = tempfile::tempdir().unwrap();
    let context = for_request(
        Some(&fixture.home),
        "node",
        Some("requester"),
        "agent",
        Some(other.path()),
        None,
    )
    .unwrap();
    let error = workspace::consent_binding(
        &approval(context.as_ref().unwrap()),
        &fixture.home,
        decision,
        false,
        None,
    )
    .await
    .unwrap_err();
    assert!(error.is::<workspace::SelectionChanged>(), "{error}");
}

#[tokio::test]
async fn unavailable_review_uses_the_same_question_for_human_consent() {
    let fixture = Fixture::new();
    let listener_home = fixture.home.clone();
    tokio::task::spawn_blocking(move || approval::mark_listening(&listener_home))
        .await
        .unwrap()
        .unwrap();
    let observed = Arc::new(Mutex::new(None));
    let reviewer: Reviewer = Arc::new({
        let observed = observed.clone();
        move |invocation| {
            *observed.lock().unwrap() = Some(invocation.action_id);
            Box::pin(async { Ok(Some(reply("another-action", "allow"))) })
        }
    });
    let review_context = fixture.context(reviewer);
    async {
        let ambient_call_197 = approval(&review_context);
        let mut consent = Box::pin(workspace::consent_binding(
            &ambient_call_197,
            &fixture.home,
            fixture.decision(&review_context, false),
            true,
            None,
        ));
        let observed_consent = futures::poll!(consent.as_mut());
        assert!(observed_consent.is_pending(), "{observed_consent:?}");
        let questions = approval::pending(&fixture.home).unwrap();
        assert_eq!(questions.len(), 1);
        assert_eq!(Some(&questions[0].id), observed.lock().unwrap().as_ref());
        approval::decide(&fixture.home, &questions[0].id, approval::Answer::Once).unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), consent)
                .await
                .unwrap()
                .unwrap()
        );
        assert!(approval::pending_requests(&fixture.home)
            .unwrap()
            .is_empty());
    }
    .await;
}

#[tokio::test]
async fn changing_permissions_during_review_cannot_authorize_the_old_action() {
    let fixture = Fixture::new();
    let release = Arc::new(tokio::sync::Notify::new());
    let reviewer: Reviewer = Arc::new({
        let release = release.clone();
        move |invocation| {
            let release = release.clone();
            Box::pin(async move {
                release.notified().await;
                Ok(Some(reply(&invocation.action_id, "allow")))
            })
        }
    });
    let review_context = fixture.context(reviewer);
    async {
        let ambient_call_238 = approval(&review_context);
        let mut consent = Box::pin(workspace::consent_binding(
            &ambient_call_238,
            &fixture.home,
            fixture.decision(&review_context, false),
            false,
            None,
        ));
        assert!(futures::poll!(consent.as_mut()).is_pending());
        workspace::save_mode(&fixture.home, &fixture.key, 1, Mode::Ask, None).unwrap();
        release.notify_one();
        assert!(consent
            .await
            .unwrap_err()
            .is::<workspace::SelectionChanged>());
        assert!(approval::pending_requests(&fixture.home)
            .unwrap()
            .is_empty());
    }
    .await;
}

#[tokio::test]
async fn a_review_that_cancels_the_caller_cannot_release_the_command() {
    let fixture = Fixture::new();
    let cancellation = tokio_util::sync::CancellationToken::new();
    let reviewer: Reviewer = Arc::new({
        let cancellation = cancellation.clone();
        move |invocation| {
            let cancellation = cancellation.clone();
            Box::pin(async move {
                cancellation.cancel();
                Ok(Some(reply(&invocation.action_id, "allow")))
            })
        }
    });
    let scope_call =
        gents_loop::tool_call_lifecycle::runtime::ToolScopeEntry::new(None, cancellation).call();
    let review_context = fixture.context(reviewer);
    async {
        let error = workspace::consent_binding(
            &scope_call
                .clone()
                .with_extension(crate::runtime_context::ToolHostContext {
                    approval: Some(review_context.clone()),
                    ..Default::default()
                }),
            &fixture.home,
            fixture.decision(&review_context, false),
            true,
            None,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("cancelled"));
        assert!(approval::pending_requests(&fixture.home)
            .unwrap()
            .is_empty());
    }
    .await;
}

#[tokio::test]
async fn stop_cancels_review_before_a_human_question_can_be_published() {
    let fixture = Fixture::new();
    let started = Arc::new(AtomicUsize::new(0));
    let reviewer: Reviewer = Arc::new({
        let started = started.clone();
        move |_| {
            started.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::pending())
        }
    });
    let cancellation = tokio_util::sync::CancellationToken::new();
    let scope_call =
        gents_loop::tool_call_lifecycle::runtime::ToolScopeEntry::new(None, cancellation.clone())
            .call();
    let review_context = fixture.context(reviewer);
    async {
        let ambient_call_309 =
            scope_call
                .clone()
                .with_extension(crate::runtime_context::ToolHostContext {
                    approval: Some(review_context.clone()),
                    ..Default::default()
                });
        let mut consent = Box::pin(workspace::consent_binding(
            &ambient_call_309,
            &fixture.home,
            fixture.decision(&review_context, false),
            true,
            None,
        ));
        assert!(futures::poll!(consent.as_mut()).is_pending());
        assert_eq!(started.load(Ordering::SeqCst), 1);
        cancellation.cancel();
        assert!(consent.await.unwrap_err().to_string().contains("cancelled"));
        assert!(approval::pending_requests(&fixture.home)
            .unwrap()
            .is_empty());
    }
    .await;
}
