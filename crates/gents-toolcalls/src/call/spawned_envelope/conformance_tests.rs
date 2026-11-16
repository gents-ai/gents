use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use gents_lean_vocab::families::tool_policy_call_envelope_derive::{Input, Seen};
use gents_loop::live_output::{LiveToolOutputRegistry, LiveToolOutputWriter};
use gents_loop::tool::{BoxFuture, ToolCallEnvelope};
use gents_loop::tool_call_lifecycle::runtime::{
    CurrentToolRuntimeContext, ToolRequestIdentity, ToolScopeEntry,
};
use gents_loop::tool_call_lifecycle::ToolOutcome;
use gents_loop::tool_effects::{ToolEffectDispatcher, ToolEffectError};
use gents_session::exec::ownership::{process_recorder, with_process_recorder, ProcessRecorder};
use gents_tools::plugin::approval::workspace::Context as ApprovalContext;
use gents_tools::runtime_context::{enter_ambient, host, ToolHostContext};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Path {
    Foreground,
    Child,
    Background,
    Handoff,
    Plugin,
    TaskHook,
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Source {
    Absent,
    Parent,
    Admitted,
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Cancel {
    Absent,
    Parent,
    ChildOfParent,
    Fresh,
}

/// Remembers the parent envelope each child call is dispatched under.
#[derive(Default)]
struct Captures(Mutex<Option<ToolCallEnvelope>>);

impl ToolEffectDispatcher for Captures {
    fn call<'a>(
        &'a self,
        parent: &'a ToolCallEnvelope,
        _ordinal: u32,
        _name: &'a str,
        _arguments: &'a str,
    ) -> BoxFuture<'a, Result<ToolOutcome, ToolEffectError>> {
        Box::pin(async move {
            *self.0.lock().unwrap() = Some(parent.clone());
            Ok(ToolOutcome::Completed("{}".to_owned()))
        })
    }
}

struct World {
    parent_deadline: DateTime<Utc>,
    admitted_deadline: DateTime<Utc>,
    writer: LiveToolOutputWriter,
    approval: ApprovalContext,
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

impl World {
    async fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let approval = gents_tools::plugin::approval::workspace::for_request(
            Some(home.path()),
            "did:key:node",
            Some("did:key:requester"),
            "did:key:agent",
            Some(workspace.path()),
            Some("session"),
        )
        .unwrap()
        .expect("a request with a home, requester and workspace has an approval context");
        Self {
            parent_deadline: Utc::now() + chrono::Duration::hours(1),
            admitted_deadline: Utc::now() + chrono::Duration::hours(2),
            writer: LiveToolOutputRegistry::default().writer_for("call").await,
            approval,
            _dirs: (home, workspace),
        }
    }

    fn parent(
        &self,
        input: &Input<Path>,
        token: &CancellationToken,
        dispatcher: Arc<dyn ToolEffectDispatcher>,
    ) -> ToolCallEnvelope {
        let runtime = input.has_scope.then(|| {
            CurrentToolRuntimeContext::entered(
                None,
                ToolScopeEntry {
                    identity: Some(ToolRequestIdentity::new(
                        Some("did:key:requester".to_owned()),
                        Some("did:key:node".to_owned()),
                        Some("did:key:agent".to_owned()),
                        Some("request".to_owned()),
                    )),
                    deadline_at: Some(self.parent_deadline),
                    cancellation_token: token.clone(),
                    workspace_cwd: Some(std::env::temp_dir()),
                    live_output: None,
                    session_id: Some("session".to_owned()),
                    correlation: None,
                    source_fields: Default::default(),
                    background: false,
                },
            )
        });
        let envelope = ToolCallEnvelope::new(runtime)
            .with_effects(input.dispatcher.then_some(dispatcher))
            .with_extension(ToolHostContext {
                workspace: Default::default(),
                interactive: input.interactive,
                approval: input.approval.then(|| self.approval.clone()),
            });
        if input.recorder {
            with_process_recorder(envelope, ProcessRecorder::new(|_| {}))
        } else {
            envelope
        }
    }

    /// The envelope the callee of `input.path` runs under, built by that
    /// path's owner inside its parent, or `None` when the owner refuses.
    async fn derive(
        &self,
        input: &Input<Path>,
        parent_token: &CancellationToken,
    ) -> Option<ToolCallEnvelope> {
        let captures = Arc::new(Captures::default());
        let parent = self.parent(input, parent_token, captures.clone());
        let offered: Option<Arc<dyn ToolEffectDispatcher>> =
            input.offered.then(|| Arc::new(Captures::default()) as _);
        enter_ambient(&parent, async {
            match input.path {
                Path::Foreground => Some(
                    gents_loop::loop_stream::foreground_envelope(
                        &parent,
                        parent.runtime(),
                        offered,
                    )
                    .0,
                ),
                Path::Child => gents_loop::loop_stream::child_envelope(&parent),
                Path::Background => Some(super::background_envelope(
                    &parent,
                    super::BackgroundAdmission {
                        request_id: "request".to_owned(),
                        deadline_at: self.admitted_deadline,
                        cancellation_token: CancellationToken::new(),
                        live_output: self.writer.clone(),
                        session_id: "session".to_owned(),
                    },
                    ProcessRecorder::new(|_| {}),
                    offered,
                    input.process,
                )),
                Path::Handoff => super::handoff_envelope(
                    &parent,
                    CancellationToken::new(),
                    ProcessRecorder::new(|_| {}),
                ),
                Path::Plugin => match crate::plugin::tool_calls::Session::for_call(true, &parent)
                {
                    Some(mut session) => {
                        session.call("child", Default::default()).await.unwrap();
                        captures.0.lock().unwrap().take()
                    }
                    None => Some(parent.clone()),
                },
                Path::TaskHook => {
                    let hook: gents_documents::TaskHook = serde_json::from_value(
                        serde_json::json!({"hook_id": "hook", "phase": "before", "command": ["true"]}),
                    )
                    .unwrap();
                    let request = crate::call::task_hooks::hook_exec_request(
                        &hook,
                        std::env::temp_dir(),
                        self.admitted_deadline,
                        CancellationToken::new(),
                        Some(ProcessRecorder::new(|_| {})),
                    );
                    let runtime = CurrentToolRuntimeContext::entered(
                        None,
                        ToolScopeEntry {
                            identity: None,
                            deadline_at: request.deadline_at,
                            cancellation_token: request.cancellation_token,
                            workspace_cwd: None,
                            live_output: None,
                            session_id: None,
                            correlation: None,
                            source_fields: Default::default(),
                            background: false,
                        },
                    );
                    let envelope = ToolCallEnvelope::new(Some(runtime));
                    Some(match request.process_recorder {
                        Some(recorder) => with_process_recorder(envelope, recorder),
                        None => envelope,
                    })
                }
            }
        })
        .await
    }

    fn deadline(&self, envelope: &ToolCallEnvelope) -> Source {
        match envelope
            .runtime_unchecked()
            .and_then(|runtime| runtime.deadline_at)
        {
            None => Source::Absent,
            Some(deadline) if deadline == self.parent_deadline => Source::Parent,
            Some(deadline) if deadline == self.admitted_deadline => Source::Admitted,
            Some(other) => panic!("unexpected deadline {other}"),
        }
    }

    /// Tokens have no identity, so the relation is observed by propagation on
    /// two derivations: cancelling the callee's token reaches the parent only
    /// when it is the parent's own, and cancelling the parent's reaches the
    /// callee unless the callee's is independent.
    async fn cancel(&self, input: &Input<Path>) -> Cancel {
        let parent = CancellationToken::new();
        let Some(callee) = self
            .derive(input, &parent)
            .await
            .and_then(|envelope| envelope.runtime_unchecked())
        else {
            return Cancel::Absent;
        };
        callee.cancellation_token.cancel();
        if parent.is_cancelled() {
            return Cancel::Parent;
        }
        let parent = CancellationToken::new();
        let callee = self
            .derive(input, &parent)
            .await
            .and_then(|envelope| envelope.runtime_unchecked())
            .expect("a derivation is a function of its input");
        parent.cancel();
        if callee.cancellation_token.is_cancelled() {
            Cancel::ChildOfParent
        } else {
            Cancel::Fresh
        }
    }

    async fn seen(&self, input: &Input<Path>) -> Option<Seen<Cancel, Source>> {
        let envelope = self.derive(input, &CancellationToken::new()).await?;
        let (host, recorder, dispatcher) = enter_ambient(&envelope, async {
            (
                host(&envelope),
                process_recorder(&envelope).is_some(),
                envelope.effects().is_some(),
            )
        })
        .await;
        Some(Seen {
            deadline: self.deadline(&envelope),
            cancel: self.cancel(input).await,
            background: envelope
                .runtime_unchecked()
                .is_some_and(|runtime| runtime.background),
            dispatcher,
            interactive: host.interactive,
            approval: host.approval.is_some(),
            recorder,
        })
    }
}

#[tokio::test]
async fn call_envelopes_derive_as_the_lean_model_says() {
    let world = World::new().await;
    for case in
        gents_lean_vocab::families::tool_policy_call_envelope_derive::cases::<Cancel, Path, Source>(
        )
    {
        let seen = world.seen(&case.input).await;
        case.check(seen);
    }
}
