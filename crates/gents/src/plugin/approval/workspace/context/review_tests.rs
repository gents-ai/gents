use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::plugin::approval;
use approval::workspace::{self, BindingAction, BindingDecision, Mode, Route};

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
                &crate::document_config::Task {
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
        )
        .unwrap()
        .unwrap()
        .with_reviewer(reviewer)
    }

    fn decision(&self, covered: bool) -> BindingDecision {
        workspace::binding_route(
            &self.home,
            || {
                Ok(BindingAction::Command {
                    argv: vec!["echo".into(), "fixture".into()],
                    cwd: self.key.path.clone(),
                    root: self.key.path.clone(),
                    writable_root: None,
                    policy: crate::toolset::CommandExecutionPolicy::write_capable(),
                })
            },
            covered,
            || Ok(serde_json::json!({"command":"echo", "args":["fixture"]})),
        )
        .unwrap()
        .unwrap()
    }
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
    scope(Some(fixture.context(reviewer)), async {
        assert!(
            workspace::consent_binding(&fixture.home, fixture.decision(true), false, None)
                .await
                .unwrap()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let mut first = Box::pin(workspace::consent_binding(
            &fixture.home,
            fixture.decision(false),
            false,
            None,
        ));
        let mut second = Box::pin(workspace::consent_binding(
            &fixture.home,
            fixture.decision(false),
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
    })
    .await;
}

#[tokio::test]
async fn a_routine_decision_cannot_be_carried_to_another_workspace() {
    let fixture = Fixture::new();
    let reviewer: Reviewer = Arc::new(|_| panic!("routine action does not need review"));
    let decision = scope(Some(fixture.context(reviewer)), async {
        fixture.decision(true)
    })
    .await;
    let other = tempfile::tempdir().unwrap();
    let context = for_request(
        Some(&fixture.home),
        "node",
        Some("requester"),
        "agent",
        Some(other.path()),
    )
    .unwrap();
    assert!(!scope(
        context,
        workspace::consent_binding(&fixture.home, decision, false, None,)
    )
    .await
    .unwrap());
}

#[tokio::test]
async fn unavailable_review_uses_the_same_question_for_human_consent() {
    let fixture = Fixture::new();
    let observed = Arc::new(Mutex::new(None));
    let reviewer: Reviewer = Arc::new({
        let observed = observed.clone();
        move |invocation| {
            *observed.lock().unwrap() = Some(invocation.action_id);
            Box::pin(async { Ok(Some(reply("another-action", "allow"))) })
        }
    });
    scope(Some(fixture.context(reviewer)), async {
        let mut consent = Box::pin(workspace::consent_binding(
            &fixture.home,
            fixture.decision(false),
            true,
            None,
        ));
        assert!(futures::poll!(consent.as_mut()).is_pending());
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
    })
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
    scope(Some(fixture.context(reviewer)), async {
        let mut consent = Box::pin(workspace::consent_binding(
            &fixture.home,
            fixture.decision(false),
            false,
            None,
        ));
        assert!(futures::poll!(consent.as_mut()).is_pending());
        workspace::save_mode(&fixture.home, &fixture.key, 1, Mode::Ask, None).unwrap();
        release.notify_one();
        assert!(matches!(
            consent.await.unwrap_err(),
            approval::PluginApprovalError::SelectionChanged
        ));
        assert!(approval::pending_requests(&fixture.home)
            .unwrap()
            .is_empty());
    })
    .await;
}

#[tokio::test]
async fn repeated_denials_return_to_a_person_without_another_review_call() {
    let fixture = Fixture::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let reviewer: Reviewer = Arc::new({
        let calls = calls.clone();
        move |invocation| {
            calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move { Ok(Some(reply(&invocation.action_id, "deny"))) })
        }
    });
    scope(Some(fixture.context(reviewer)), async {
        for expected in [Route::Deny, Route::Deny, Route::Deny, Route::Ask] {
            let decision = fixture.decision(false);
            let question = approval::Request::for_binding(decision.binding.clone(), None);
            assert_eq!(
                resolve_review(&fixture.home, &decision, &question.id)
                    .await
                    .unwrap(),
                expected
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    })
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
    scope(
        Some(fixture.context(reviewer)),
        crate::tool_call_lifecycle::runtime::scope_request_tool_execution(
            None,
            cancellation,
            async {
                let error =
                    workspace::consent_binding(&fixture.home, fixture.decision(false), true, None)
                        .await
                        .unwrap_err();
                assert!(error.to_string().contains("cancelled"));
                assert!(approval::pending_requests(&fixture.home)
                    .unwrap()
                    .is_empty());
            },
        ),
    )
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
    scope(
        Some(fixture.context(reviewer)),
        crate::tool_call_lifecycle::runtime::scope_request_tool_execution(
            None,
            cancellation.clone(),
            async {
                let mut consent = Box::pin(workspace::consent_binding(
                    &fixture.home,
                    fixture.decision(false),
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
            },
        ),
    )
    .await;
}
