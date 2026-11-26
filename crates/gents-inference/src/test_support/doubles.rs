use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use bytes::Bytes;
use futures::future::BoxFuture;
use gents_loop::provider_call::ProviderCall;
use gents_loop::rig_compat::CachedInputTokensObservation;
use gents_loop::tool::{Tool, ToolCallEnvelope, ToolDefinition};
use gents_store::config_client::ConfigAccess;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::oneshot;

use crate::admission::permit::AdmissionPermit;
use crate::admission::stream_guard::StreamGuardLifecycle;
use crate::admission::{AdmissionError, ProviderCallUsage};
use crate::auth::credential::BearerSource;
use crate::llm::rig_compat::http::{
    self as http_client, BoxedStream, HeaderMap, LazyBody, MultipartForm, Request, Response,
    StreamingResponse, Transport,
};
use crate::rendered_request::{encoding, CaptureBaseReader};

/// Returns scripted tokens in order, then repeats the last; `Err` entries fail the refresh.
pub(crate) struct ScriptedBearer {
    script: Mutex<VecDeque<Result<String, String>>>,
    last: Mutex<Result<String, String>>,
    pub(crate) invalidations: AtomicUsize,
}

impl ScriptedBearer {
    pub(crate) fn new(script: &[Result<&str, &str>]) -> Self {
        let owned: VecDeque<Result<String, String>> = script
            .iter()
            .map(|entry| entry.map(str::to_owned).map_err(str::to_owned))
            .collect();
        Self {
            last: Mutex::new(owned.front().cloned().expect("script")),
            script: Mutex::new(owned),
            invalidations: AtomicUsize::new(0),
        }
    }
}

impl BearerSource for ScriptedBearer {
    async fn current_bearer(&self) -> Result<String> {
        let mut script = self.script.lock().expect("script");
        let next = script
            .pop_front()
            .unwrap_or_else(|| self.last.lock().expect("last").clone());
        *self.last.lock().expect("last") = next.clone();
        next.map_err(|message| anyhow::anyhow!(message))
    }

    async fn invalidate(&self) {
        self.invalidations.fetch_add(1, Ordering::SeqCst);
    }
}

pub(crate) type BlockAnswer = Option<(Vec<u8>, String)>;

fn commit_query_aliases(query: &str) -> Vec<(String, String)> {
    let parts = query.split(": _commits(docID: \"").collect::<Vec<_>>();
    parts
        .windows(2)
        .filter_map(|pair| {
            let alias = pair[0].split_whitespace().last()?;
            let (doc_id, _) = pair[1].split_once('"')?;
            Some((alias.to_owned(), doc_id.to_owned()))
        })
        .collect()
}

fn commit_response(
    aliases: Vec<(String, String)>,
    cid_of: impl Fn(&str) -> Option<String>,
) -> Value {
    let data = aliases
        .into_iter()
        .map(|(alias, doc_id)| {
            let commits = cid_of(&doc_id)
                .map(|cid| serde_json::json!([{ "cid": cid, "height": 1, "fieldName": "payload" }]))
                .unwrap_or_else(|| serde_json::json!([]));
            (alias, commits)
        })
        .collect::<serde_json::Map<_, _>>();
    serde_json::json!({ "data": data })
}

pub(crate) struct ManifestStore {
    pub(crate) fail_queries: bool,
    blocks: Vec<(String, String, Vec<u8>)>,
    answers: BTreeMap<usize, BlockAnswer>,
}

impl ManifestStore {
    pub(crate) fn healthy(blocks: &[(String, String, Vec<u8>)]) -> Self {
        Self {
            fail_queries: false,
            blocks: blocks.to_vec(),
            answers: BTreeMap::new(),
        }
    }

    pub(crate) fn broken(
        blocks: &[(String, String, Vec<u8>)],
        answers: BTreeMap<usize, BlockAnswer>,
    ) -> Self {
        Self {
            fail_queries: false,
            blocks: blocks.to_vec(),
            answers,
        }
    }
}

#[async_trait::async_trait]
impl CaptureBaseReader for ManifestStore {
    async fn execute_capture_query(&self, query: &str) -> Result<Value> {
        if self.fail_queries {
            anyhow::bail!("store unavailable");
        }
        let aliases = commit_query_aliases(query);
        if !aliases.is_empty() {
            return Ok(commit_response(aliases, |doc_id| {
                let index = doc_id.strip_prefix("block-")?.parse::<usize>().ok()?;
                Some(
                    self.answers
                        .get(&index)
                        .cloned()
                        .flatten()
                        .map(|(_, cid)| cid)
                        .unwrap_or_else(|| self.blocks[index].0.clone()),
                )
            }));
        }
        if !query.contains("RenderedRequestBlock") {
            return Ok(serde_json::json!({"data": {"RenderedRequest": []}}));
        }
        let rows = self
            .blocks
            .iter()
            .enumerate()
            .filter_map(|(index, (_, key, bytes))| {
                let answer = self
                    .answers
                    .get(&index)
                    .cloned()
                    .unwrap_or_else(|| Some((bytes.clone(), String::new())));
                let (bytes, _) = answer?;
                Some(serde_json::json!({
                    "_docID": format!("block-{index}"),
                    "payload": String::from_utf8(bytes.clone()).unwrap(),
                    "byte_len": bytes.len(),
                    "content_key": key,
                }))
            })
            .collect::<Vec<_>>();
        Ok(serde_json::json!({"data": {"RenderedRequestBlock": rows}}))
    }
}

pub(crate) struct DeltaBaseStore {
    pub(crate) readable: bool,
    pub(crate) base: Option<(&'static str, &'static str)>,
}

pub(crate) fn delta_base_value() -> Value {
    serde_json::json!({"model": "m", "stream": false, "request": 7, "padding": "x".repeat(4096)})
}

#[async_trait::async_trait]
impl CaptureBaseReader for DeltaBaseStore {
    async fn execute_capture_query(&self, _query: &str) -> Result<Value> {
        anyhow::ensure!(self.readable, "store unavailable");
        let Some((cid, session)) = self.base else {
            return Ok(serde_json::json!({"data": {"RenderedRequest": []}}));
        };
        let encoded = encoding::encode_container(
            &encoding::encode_full(&delta_base_value())?,
            &encoding::encode_full(&serde_json::json!({}))?,
        )?;
        Ok(serde_json::json!({"data":{"RenderedRequest":[{
            "capture_version":2, "node_did":"did:test", "requester_did":"",
            "session_id":session, "source":"openai_responses", "capture_scope":"inference.1",
            "request_json":encoded
        }], "_commits":[{"cid":cid, "height":1, "fieldName":"request_json"}]}}))
    }
}

pub(crate) struct MisreferencingBlockStore {
    pub(crate) docs: Vec<(String, String, String)>,
}

#[async_trait::async_trait]
impl CaptureBaseReader for MisreferencingBlockStore {
    async fn execute_capture_query(&self, query: &str) -> Result<Value> {
        let aliases = commit_query_aliases(query);
        if !aliases.is_empty() {
            return Ok(commit_response(aliases, |doc_id| {
                self.docs
                    .iter()
                    .find(|(named, _, _)| named == doc_id)
                    .map(|(_, _, cid)| cid.clone())
            }));
        }
        if !query.contains("RenderedRequestBlock") {
            return Ok(serde_json::json!({"data": {"RenderedRequest": []}}));
        }
        let rows = self
            .docs
            .iter()
            .map(|(doc_id, payload, _)| {
                serde_json::json!({
                    "_docID": doc_id,
                    "payload": payload,
                    "byte_len": payload.len(),
                })
            })
            .collect::<Vec<_>>();
        Ok(serde_json::json!({"data": {"RenderedRequestBlock": rows}}))
    }
}

/// Records headers and JSON request bodies it is sent, and answers each
/// streaming send with the next of `replies`' headers and an ended stream.
#[derive(Clone, Debug, Default)]
pub(crate) struct RecordingTransport {
    sent: Arc<Mutex<Vec<HeaderMap>>>,
    bodies: Arc<Mutex<Vec<Value>>>,
    replies: Arc<Mutex<Vec<HeaderMap>>>,
}

impl RecordingTransport {
    pub(crate) fn replying(replies: Vec<HeaderMap>) -> Self {
        Self {
            sent: Arc::default(),
            bodies: Arc::default(),
            replies: Arc::new(Mutex::new(replies.into_iter().rev().collect())),
        }
    }

    pub(crate) fn sent(&self) -> Vec<HeaderMap> {
        self.sent.lock().unwrap().clone()
    }

    pub(crate) fn bodies(&self) -> Vec<Value> {
        self.bodies.lock().expect("capture lock").clone()
    }
}

impl Transport for RecordingTransport {
    fn send(
        &self,
        _call: &ProviderCall,
        request: Request<Bytes>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<Bytes>>>> + Send + 'static {
        self.sent.lock().unwrap().push(request.headers().clone());
        let body = request.into_body();
        if let Ok(value) = serde_json::from_slice(&body) {
            self.bodies.lock().expect("capture lock").push(value);
        }
        async move {
            let body: LazyBody<Bytes> = Box::pin(async { Ok(Bytes::from_static(b"{}")) });
            Ok(Response::builder().status(200).body(body)?)
        }
    }

    fn send_multipart(
        &self,
        _call: &ProviderCall,
        request: Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<Bytes>>>> + Send + 'static {
        self.sent.lock().unwrap().push(request.headers().clone());
        async move {
            let body: LazyBody<Bytes> = Box::pin(async { Ok(Bytes::from_static(b"{}")) });
            Ok(Response::builder().status(200).body(body)?)
        }
    }

    fn send_streaming(
        &self,
        _call: &ProviderCall,
        request: Request<Bytes>,
    ) -> impl Future<Output = http_client::Result<StreamingResponse>> + Send {
        self.sent.lock().unwrap().push(request.headers().clone());
        let headers = self.replies.lock().unwrap().pop().unwrap_or_default();
        let body = request.into_body();
        if let Ok(value) = serde_json::from_slice(&body) {
            self.bodies.lock().expect("capture lock").push(value);
        }
        async move {
            let body: BoxedStream = Box::pin(futures::stream::iter([Ok(Bytes::from_static(
                b"data: [DONE]\n\n",
            ))]));
            let mut response = Response::builder()
                .status(200)
                .header("content-type", "text/event-stream");
            if let Some(map) = response.headers_mut() {
                map.extend(headers);
            }
            Ok(response.body(body)?)
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct PostStatusArgs {
    report_type: String,
    scope: String,
    body: String,
    findings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct PostStatusOutput {
    posted: bool,
    report: PostStatusArgs,
}

#[derive(Debug, thiserror::Error)]
#[error("post_status tool failure: {0}")]
pub(crate) struct PostStatusError(String);

pub(crate) struct PostStatusTool;

impl Tool for PostStatusTool {
    const NAME: &'static str = "post_status";
    type Error = PostStatusError;
    type Args = PostStatusArgs;
    type Output = PostStatusOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Post a steward status report.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "report_type": {"type": "string"},
                    "scope": {"type": "string"},
                    "body": {"type": "string"},
                    "findings": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["report_type", "scope", "body", "findings"]
            }),
        }
    }

    async fn call(
        &self,
        args: Self::Args,
        _context: &ToolCallEnvelope,
    ) -> std::result::Result<Self::Output, Self::Error> {
        Ok(PostStatusOutput {
            posted: true,
            report: args,
        })
    }
}

pub(crate) struct GuardDrop(pub(crate) Arc<AtomicUsize>);

impl Drop for GuardDrop {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

pub(crate) struct Finalizer {
    pub(crate) gate: Arc<tokio::sync::Mutex<()>>,
    pub(crate) entered: oneshot::Sender<()>,
    pub(crate) storage_return: oneshot::Receiver<()>,
    pub(crate) finished: Arc<AtomicBool>,
    pub(crate) guard: GuardDrop,
}

impl StreamGuardLifecycle for Finalizer {
    fn finish_stream(self) -> BoxFuture<'static, std::result::Result<(), AdmissionError>> {
        Box::pin(async move {
            let _guard_drop = self.guard;
            let _guard = self.gate.lock().await;
            let _ = self.entered.send(());
            let _ = self.storage_return.await;
            self.finished.store(true, Ordering::SeqCst);
            Ok(())
        })
    }
}

pub(crate) struct NativeFinalizer {
    pub(crate) node: Arc<defra_node::Node>,
    pub(crate) entered: Arc<tokio::sync::Notify>,
    pub(crate) storage_return: Arc<tokio::sync::Notify>,
}

impl StreamGuardLifecycle for NativeFinalizer {
    fn finish_stream(self) -> BoxFuture<'static, std::result::Result<(), AdmissionError>> {
        Box::pin(async move {
            ConfigAccess::transact_local_write(
                &self.node,
                None,
                "test.stream_finalization.native_owner",
                move |txn| {
                    let entered = self.entered.clone();
                    let storage_return = self.storage_return.clone();
                    Box::pin(async move {
                        entered.notify_one();
                        storage_return.notified().await;
                        txn.execute(
                            "mutation { create_StreamGateFact(input: {value: \"finalizer\"}) { _docID } }",
                        )
                        .await?;
                        Ok::<_, anyhow::Error>(())
                    })
                },
            )
            .await
            .map_err(|error| AdmissionError(error.to_string()))
        })
    }
}

pub(crate) struct DropProbe {
    pub(crate) drops: Arc<AtomicUsize>,
}

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

impl StreamGuardLifecycle for DropProbe {}

pub(crate) struct AsyncFinalizeProbe {
    pub(crate) started: Arc<AtomicBool>,
    pub(crate) finished: Arc<AtomicBool>,
    pub(crate) release: oneshot::Receiver<()>,
    pub(crate) error: Option<&'static str>,
}

impl StreamGuardLifecycle for AsyncFinalizeProbe {
    fn finish_stream(self) -> BoxFuture<'static, std::result::Result<(), AdmissionError>> {
        Box::pin(async move {
            self.started.store(true, Ordering::SeqCst);
            let _ = self.release.await;
            self.finished.store(true, Ordering::SeqCst);
            match self.error {
                Some(error) => Err(AdmissionError(error.to_string())),
                None => Ok(()),
            }
        })
    }
}

pub(crate) struct SignalledPermitFinalizer {
    pub(crate) permit: AdmissionPermit,
    pub(crate) invoked: oneshot::Sender<()>,
}

impl StreamGuardLifecycle for SignalledPermitFinalizer {
    fn mark_stream_success(
        &mut self,
        usage: Option<ProviderCallUsage>,
        cached_input_tokens: CachedInputTokensObservation,
    ) {
        StreamGuardLifecycle::mark_stream_success(&mut self.permit, usage, cached_input_tokens);
    }

    fn mark_stream_error(&mut self, error: &str) {
        StreamGuardLifecycle::mark_stream_error(&mut self.permit, error);
    }

    fn finish_stream(self) -> BoxFuture<'static, std::result::Result<(), AdmissionError>> {
        Box::pin(async move {
            let _ = self.invoked.send(());
            StreamGuardLifecycle::finish_stream(self.permit).await
        })
    }
}
