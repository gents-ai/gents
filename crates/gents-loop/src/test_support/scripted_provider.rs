//! The one model tests stand in for a provider with. Its behaviour is data:
//! the [`Reply`] each call answers with, played back as [`Step`]s, and a
//! [`CallRecord`] log the test reads or waits on.

use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures::future::BoxFuture;
use futures::StreamExt;
use rig::completion::{CompletionError, CompletionRequest};
use rig::streaming::RawStreamingChoice;
use tokio::sync::{watch, Notify};
use tokio_util::sync::CancellationToken;

use crate::provider_call::ProviderCall;
use crate::rig_compat::model::{LiftedModel, Model, ModelStream, StreamEnd};

/// The scripted provider as the completion model the loop drives.
pub type ScriptedProvider = LiftedModel<Script>;

/// One element of a reply's stream, played in order.
pub enum Step {
    /// Yields this chunk; `FinalResponse(())` ends with no usage.
    Chunk(RawStreamingChoice<()>),
    /// Yields a final response carrying this end.
    End(StreamEnd),
    /// Yields this error as the next item.
    Error(CompletionError),
    /// Notifies once when playback reaches it.
    Signal(Arc<Notify>),
    /// Holds playback until notified.
    Wait(Arc<Notify>),
    /// Holds playback for this long, as a provider paces its chunks.
    Delay(Duration),
    /// Yields `chunk` every `every` until `until` is cancelled, which ends
    /// the stream with no terminal response; forever without one.
    Repeat {
        chunk: RawStreamingChoice<()>,
        every: Duration,
        until: Option<CancellationToken>,
    },
    /// Never yields again.
    Pending,
}

impl Step {
    /// A final response reporting `usage` the way a provider's
    /// `{"usage": ...}` payload reads.
    pub fn usage_end(usage: rig::completion::Usage) -> Self {
        Self::End(StreamEnd {
            usage: Some(usage),
            token_usage: crate::rig_compat::observed_token_usage(
                &serde_json::json!({ "usage": usage }),
            ),
            request_id: None,
        })
    }
}

/// How one provider call answers.
pub enum Reply {
    /// The call opens a stream that plays these steps.
    Stream(Vec<Step>),
    /// The call fails before a stream opens.
    Fail(CompletionError),
}

impl Reply {
    pub fn turn(chunks: Vec<RawStreamingChoice<()>>) -> Self {
        Self::Stream(chunks.into_iter().map(Step::Chunk).collect())
    }

    /// A text message and a final response.
    pub fn text(text: impl Into<String>) -> Self {
        Self::turn(vec![
            RawStreamingChoice::Message(text.into()),
            RawStreamingChoice::FinalResponse(()),
        ])
    }

    /// A final response alone.
    pub fn end() -> Self {
        Self::turn(vec![RawStreamingChoice::FinalResponse(())])
    }

    pub fn provider_error(message: impl Into<String>) -> Self {
        Self::Fail(CompletionError::ProviderError(message.into()))
    }

    /// `chunks`, then a stream that never yields again.
    pub fn stalled(chunks: Vec<RawStreamingChoice<()>>) -> Self {
        let mut steps = chunks.into_iter().map(Step::Chunk).collect::<Vec<_>>();
        steps.push(Step::Pending);
        Self::Stream(steps)
    }

    /// `chunks`, then `error` as the stream's next item.
    pub fn failing_mid_stream(chunks: Vec<RawStreamingChoice<()>>, error: CompletionError) -> Self {
        let mut steps = chunks.into_iter().map(Step::Chunk).collect::<Vec<_>>();
        steps.push(Step::Error(error));
        Self::Stream(steps)
    }
}

impl From<Vec<RawStreamingChoice<()>>> for Reply {
    fn from(chunks: Vec<RawStreamingChoice<()>>) -> Self {
        Self::turn(chunks)
    }
}

/// One provider call as the provider received it.
#[derive(Clone)]
pub struct Dispatch {
    /// Its position among this provider's calls, from zero.
    pub index: usize,
    pub request: CompletionRequest,
    pub call: ProviderCall,
    /// The OpenAI wire body sent through the capture transport, when the
    /// provider captures that way.
    pub body: Option<serde_json::Value>,
}

/// A call and what its stream has done so far.
#[derive(Clone)]
pub struct CallRecord {
    pub dispatch: Dispatch,
    /// Items its stream has yielded.
    pub yielded: usize,
    /// Its stream ended or was dropped.
    pub closed: bool,
}

/// Holds each call at dispatch, before its request is sent: `entered` is
/// notified, then the call waits for `release`.
#[derive(Default)]
pub struct DispatchGate {
    pub entered: Notify,
    pub release: Notify,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Observed {
    Dispatch,
    Poll,
}

/// Sees each dispatch and each yielded item with the call it belongs to.
pub type Observer = Arc<dyn Fn(Observed, &ProviderCall) + Send + Sync>;

type Respond = Arc<dyn Fn(Dispatch) -> BoxFuture<'static, Reply> + Send + Sync>;

enum Capture {
    None,
    /// Claims the call's armed capture as a real transport send would.
    ClaimPending,
    /// Sends the request's OpenAI wire body for this model through the
    /// capturing transport.
    #[cfg(feature = "native")]
    OpenAi(String),
}

struct Shared {
    queue: Mutex<VecDeque<Reply>>,
    respond: Option<Respond>,
    capture: Capture,
    gate: Option<Arc<DispatchGate>>,
    observer: Option<Observer>,
    log: Mutex<Vec<CallRecord>>,
    changed: watch::Sender<()>,
}

/// The scripted provider's state, shared by every per-call copy.
#[derive(Clone)]
pub struct Script {
    shared: Arc<Shared>,
    call: ProviderCall,
}

impl fmt::Debug for Script {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Script")
            .field("calls", &lock(&self.shared.log).len())
            .finish()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Script {
    fn new(replies: VecDeque<Reply>) -> Self {
        Self {
            shared: Arc::new(Shared {
                queue: Mutex::new(replies),
                respond: None,
                capture: Capture::None,
                gate: None,
                observer: None,
                log: Mutex::new(Vec::new()),
                changed: watch::channel(()).0,
            }),
            call: ProviderCall::default(),
        }
    }

    fn changed(&self) {
        self.shared.changed.send_replace(());
    }

    async fn capture(
        &self,
        #[cfg_attr(not(feature = "native"), expect(unused_variables))] request: &CompletionRequest,
    ) -> Result<Option<serde_json::Value>, CompletionError> {
        match &self.shared.capture {
            Capture::None => Ok(None),
            Capture::ClaimPending => {
                let _ = crate::rendered_request::scope::claim_pending(self.call.capture.as_ref());
                Ok(None)
            }
            #[cfg(feature = "native")]
            Capture::OpenAi(model) => openai_capture(&self.call, request, model).await.map(Some),
        }
    }
}

#[cfg(feature = "native")]
async fn openai_capture(
    call: &ProviderCall,
    request: &CompletionRequest,
    model: &str,
) -> Result<serde_json::Value, CompletionError> {
    let provider_error =
        |error: &dyn fmt::Display| CompletionError::ProviderError(error.to_string());
    let body = rig::providers::openai::completion::CompletionRequest::try_from((
        model.to_owned(),
        request.clone(),
    ))
    .map_err(|error| provider_error(&error))?;
    let mut body = serde_json::to_value(body).map_err(|error| provider_error(&error))?;
    body["stream"] = serde_json::Value::Bool(true);
    body["stream_options"] = serde_json::json!({ "include_usage": true });
    let inner = crate::rendered_request::transport::CountingInner::default();
    let transport =
        crate::rendered_request::transport::RenderedRequestCapturingHttpClient::new(inner.clone());
    let outbound = rig::http_client::Request::builder()
        .method("POST")
        .uri("https://scripted-provider.invalid/v1/chat/completions")
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).map_err(|error| provider_error(&error))?,
        ))
        .map_err(|error| provider_error(&error))?;
    crate::rig_compat::http::Transport::send_streaming(&transport, call, outbound)
        .await
        .map_err(|error| provider_error(&error))?;
    assert_eq!(
        inner.send_count(),
        1,
        "capture must authorize the scripted provider send"
    );
    Ok(body)
}

impl Model for Script {
    type Client = ();

    fn make(_: &(), _: String) -> Self {
        Self::new(VecDeque::new())
    }

    fn for_call(&self, call: &ProviderCall) -> Self {
        Self {
            shared: self.shared.clone(),
            call: call.clone(),
        }
    }

    async fn completion(
        &self,
        request: CompletionRequest,
    ) -> Result<(String, StreamEnd), CompletionError> {
        let mut stream = self.stream(request).await?;
        let mut text = String::new();
        let mut end = StreamEnd::default();
        while let Some(item) = stream.next().await {
            match item? {
                RawStreamingChoice::Message(chunk) => text.push_str(&chunk),
                RawStreamingChoice::FinalResponse(final_end) => end = final_end,
                _ => {}
            }
        }
        Ok((text, end))
    }

    async fn stream(&self, request: CompletionRequest) -> Result<ModelStream, CompletionError> {
        if let Some(observer) = &self.shared.observer {
            observer(Observed::Dispatch, &self.call);
        }
        if let Some(gate) = &self.shared.gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        let body = self.capture(&request).await?;
        let dispatch = {
            let mut log = lock(&self.shared.log);
            let dispatch = Dispatch {
                index: log.len(),
                request,
                call: self.call.clone(),
                body,
            };
            log.push(CallRecord {
                dispatch: dispatch.clone(),
                yielded: 0,
                closed: false,
            });
            dispatch
        };
        self.changed();
        let index = dispatch.index;
        let queued = lock(&self.shared.queue).pop_front();
        let reply = match (queued, &self.shared.respond) {
            (Some(reply), _) => reply,
            (None, Some(respond)) => respond(dispatch).await,
            (None, None) => panic!(
                "the scripted provider was called {} times, past its script",
                index + 1
            ),
        };
        let steps = match reply {
            Reply::Fail(error) => return Err(error),
            Reply::Stream(steps) => steps,
        };
        let playback = Playback {
            script: self.clone(),
            index,
            steps: steps.into(),
        };
        Ok(Box::pin(futures::stream::unfold(
            playback,
            |mut playback| async move {
                let item = playback.next().await?;
                Some((item, playback))
            },
        )))
    }
}

struct Playback {
    script: Script,
    index: usize,
    steps: VecDeque<Step>,
}

impl Playback {
    async fn next(&mut self) -> Option<Result<RawStreamingChoice<StreamEnd>, CompletionError>> {
        let item = loop {
            match self.steps.pop_front()? {
                Step::Chunk(chunk) => break Ok(ended(chunk)),
                Step::End(end) => break Ok(RawStreamingChoice::FinalResponse(end)),
                Step::Error(error) => break Err(error),
                Step::Signal(notify) => notify.notify_one(),
                Step::Wait(notify) => notify.notified().await,
                Step::Delay(delay) => tokio::time::sleep(delay).await,
                Step::Pending => std::future::pending::<()>().await,
                Step::Repeat {
                    chunk,
                    every,
                    until,
                } => {
                    match &until {
                        Some(until) => tokio::select! {
                            () = until.cancelled() => return None,
                            () = tokio::time::sleep(every) => {}
                        },
                        None => tokio::time::sleep(every).await,
                    }
                    let item = ended(chunk.clone());
                    self.steps.push_front(Step::Repeat {
                        chunk,
                        every,
                        until,
                    });
                    break Ok(item);
                }
            }
        };
        lock(&self.script.shared.log)[self.index].yielded += 1;
        self.script.changed();
        if let Some(observer) = &self.script.shared.observer {
            observer(Observed::Poll, &self.script.call);
        }
        Some(item)
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        lock(&self.script.shared.log)[self.index].closed = true;
        self.script.changed();
    }
}

fn ended(chunk: RawStreamingChoice<()>) -> RawStreamingChoice<StreamEnd> {
    match chunk {
        RawStreamingChoice::Message(text) => RawStreamingChoice::Message(text),
        RawStreamingChoice::ToolCall(call) => RawStreamingChoice::ToolCall(call),
        RawStreamingChoice::ToolCallDelta {
            id,
            internal_call_id,
            content,
        } => RawStreamingChoice::ToolCallDelta {
            id,
            internal_call_id,
            content,
        },
        RawStreamingChoice::Reasoning { id, content } => {
            RawStreamingChoice::Reasoning { id, content }
        }
        RawStreamingChoice::ReasoningDelta { id, reasoning } => {
            RawStreamingChoice::ReasoningDelta { id, reasoning }
        }
        RawStreamingChoice::FinalResponse(()) => {
            RawStreamingChoice::FinalResponse(StreamEnd::default())
        }
        RawStreamingChoice::MessageId(id) => RawStreamingChoice::MessageId(id),
    }
}

impl LiftedModel<Script> {
    /// Answers its calls with `replies` in order; a call past them panics
    /// unless [`respond`](Self::respond) says otherwise.
    pub fn new(replies: impl IntoIterator<Item = Reply>) -> Self {
        Self(Script::new(replies.into_iter().collect()))
    }

    /// Answers each call with the next of `turns`.
    pub fn turns(turns: Vec<Vec<RawStreamingChoice<()>>>) -> Self {
        Self::new(turns.into_iter().map(Reply::turn))
    }

    /// Answers its first call with `chunks`.
    pub fn turn(chunks: Vec<RawStreamingChoice<()>>) -> Self {
        Self::turns(vec![chunks])
    }

    /// Answers every call from what it received.
    pub fn responding<F>(respond: F) -> Self
    where
        F: Fn(&Dispatch) -> Reply + Send + Sync + 'static,
    {
        Self::new([]).respond(respond)
    }

    fn configure(mut self, configure: impl FnOnce(&mut Shared)) -> Self {
        configure(
            Arc::get_mut(&mut self.0.shared)
                .expect("configure the scripted provider before cloning it"),
        );
        self
    }

    /// Answers each call past the scripted replies from what it received.
    pub fn respond<F>(self, respond: F) -> Self
    where
        F: Fn(&Dispatch) -> Reply + Send + Sync + 'static,
    {
        self.respond_async(move |dispatch| std::future::ready(respond(&dispatch)))
    }

    /// [`respond`](Self::respond), awaiting what each answer needs first.
    pub fn respond_async<F, Fut>(self, respond: F) -> Self
    where
        F: Fn(Dispatch) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Reply> + Send + 'static,
    {
        self.configure(|shared| {
            shared.respond = Some(Arc::new(move |dispatch| Box::pin(respond(dispatch))));
        })
    }

    /// Answers each call past the scripted replies with a final response
    /// alone.
    pub fn then_end(self) -> Self {
        self.respond(|_| Reply::end())
    }

    /// Claims each call's armed capture, as a real transport send would.
    pub fn claiming_capture(self) -> Self {
        self.configure(|shared| shared.capture = Capture::ClaimPending)
    }

    /// Sends each call's OpenAI wire body for `model` through the capturing
    /// transport, which makes the call's capture durable.
    #[cfg(feature = "native")]
    pub fn capturing_openai(self, model: &str) -> Self {
        let model = model.to_owned();
        self.configure(|shared| shared.capture = Capture::OpenAi(model))
    }

    pub fn gated(self, gate: Arc<DispatchGate>) -> Self {
        self.configure(|shared| shared.gate = Some(gate))
    }

    pub fn observed(self, observer: Observer) -> Self {
        self.configure(|shared| shared.observer = Some(observer))
    }

    pub fn calls(&self) -> usize {
        lock(&self.0.shared.log).len()
    }

    pub fn records(&self) -> Vec<CallRecord> {
        lock(&self.0.shared.log).clone()
    }

    pub fn requests(&self) -> Vec<CompletionRequest> {
        lock(&self.0.shared.log)
            .iter()
            .map(|record| record.dispatch.request.clone())
            .collect()
    }

    /// Each call's chat history as native messages.
    pub fn histories(&self) -> Vec<Vec<gents_protocol::message::Message>> {
        lock(&self.0.shared.log)
            .iter()
            .map(|record| {
                record
                    .dispatch
                    .request
                    .chat_history
                    .iter()
                    .map(crate::rig_compat::from_rig_message)
                    .collect()
            })
            .collect()
    }

    /// Returns once `done` holds of the call log, checked after each call
    /// and each yielded item.
    pub async fn wait_until<F>(&self, done: F)
    where
        F: Fn(&[CallRecord]) -> bool,
    {
        let mut changed = self.0.shared.changed.subscribe();
        loop {
            let reached = done(&lock(&self.0.shared.log));
            if reached {
                return;
            }
            changed
                .changed()
                .await
                .expect("the provider holds its change sender");
        }
    }

    pub async fn wait_for_calls(&self, calls: usize) {
        self.wait_until(|log| log.len() >= calls).await;
    }
}
