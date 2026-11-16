use std::collections::BTreeSet;
use std::sync::{Arc, LazyLock};
use std::time::Instant;

use anyhow::{Context, Result};
use futures::StreamExt;
use serde_json::{json, Map, Value};
use tokio::sync::Semaphore;

use gents_documents::InferenceBackend;
use gents_pack::plugin::model_binding::{ModelEndpoint, ModelProfileVisibility};
use gents_store::collection::Collection;

pub const MAX_REQUESTS_PER_ROUND: usize = 64;
pub const MAX_RESULT_BYTES: usize = 1024 * 1024;
pub const MAX_REQUESTS_PER_CALL: usize = 512;
pub const MAX_ANSWER_BYTES_PER_CALL: usize = 32 * 1024 * 1024;
pub const DEFAULT_MAX_TOKENS: u64 = 8192;
pub const DEAD_ENDPOINT_ROUNDS: u32 = 2;
async fn check_profile_visibility(visibility: &ModelProfileVisibility) -> Result<Option<String>> {
    let backend = visibility
        .access
        .transact_readonly("plugin.model_recheck", |txn| {
            Box::pin(async move {
                let profile = gents_documents::read_desired_state_record_in_txn(
                    txn,
                    Collection::InferenceProfile,
                    &visibility.owner,
                    &visibility.profile_id,
                )
                .await?
                .context("selected profile is unavailable")?;
                let backend = gents_documents::read_desired_state_record_in_txn(
                    txn,
                    Collection::InferenceBackend,
                    &visibility.owner,
                    &visibility.backend_id,
                )
                .await?
                .context("selected backend is unavailable")?;
                anyhow::ensure!(
                    profile.0 == visibility.profile_doc
                        && profile.1 == visibility.profile_value
                        && backend.0 == visibility.backend_doc
                        && backend.1 == visibility.backend_value,
                    "selected model configuration changed"
                );
                Ok::<_, anyhow::Error>(serde_json::from_value::<InferenceBackend>(backend.1)?)
            })
        })
        .await?;
    if !matches!(
        &backend.auth,
        gents_documents::BackendAuth::Credential { .. }
    ) {
        return Ok(None);
    }
    let node = visibility
        .access
        .local_node()
        .context("runtime credential resolver unavailable")?;
    let auth = gents_inference::auth::credential::resolve_runtime_backend_auth(
        node,
        &backend.node_did,
        backend.provider_kind,
        &backend.endpoint,
        &backend.auth,
    )
    .await?;
    auth.resolve_api_key()
}

struct Image {
    mime: String,
    data_base64: String,
}

pub struct Request {
    id: String,
    prompt: String,
    images: Vec<Image>,
    max_tokens: Option<u64>,
}

pub struct Batch {
    pub requests: Vec<Request>,
    pub state: Option<Value>,
}

pub fn parse_batch(output: &Value) -> Result<Option<Batch>, String> {
    let Some(calls) = output
        .as_object()
        .and_then(|object| object.get("model_calls"))
        .and_then(Value::as_object)
    else {
        return Ok(None);
    };
    let requests = calls
        .get("requests")
        .and_then(Value::as_array)
        .ok_or("model_calls.requests must be an array")?;
    if requests.len() > MAX_REQUESTS_PER_ROUND {
        return Err(format!(
            "model_calls.requests holds {} requests; one round takes at most {MAX_REQUESTS_PER_ROUND}",
            requests.len()
        ));
    }
    let mut ids = BTreeSet::new();
    let requests = requests
        .iter()
        .map(|request| {
            let text = |key: &str| request.get(key).and_then(Value::as_str);
            let id = text("id")
                .filter(|id| !id.is_empty())
                .ok_or("every model request needs a non-empty string id")?;
            if !ids.insert(id) {
                return Err(format!("model request id {id:?} is used twice in one round"));
            }
            let prompt = text("prompt").ok_or_else(|| format!("model request {id:?} needs a prompt"))?;
            let images = match request.get("images") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::Array(images)) => images
                    .iter()
                    .map(|image| {
                        let mime = image.get("mime").and_then(Value::as_str);
                        let data = image.get("data_base64").and_then(Value::as_str);
                        match (mime, data) {
                            (Some(mime @ ("image/png" | "image/jpeg")), Some(data))
                                if !data.is_empty() =>
                            {
                                Ok(Image {
                                    mime: mime.to_owned(),
                                    data_base64: data.to_owned(),
                                })
                            }
                            _ => Err(format!(
                                "model request {id:?} has an image that is not image/png or image/jpeg with data_base64"
                            )),
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                Some(_) => return Err(format!("model request {id:?} images must be an array")),
            };
            let max_tokens = match request.get("max_tokens") {
                None | Some(Value::Null) => None,
                Some(value) => Some(
                    value
                        .as_u64()
                        .filter(|tokens| *tokens > 0)
                        .ok_or_else(|| format!("model request {id:?} max_tokens must be a positive integer"))?,
                ),
            };
            Ok(Request {
                id: id.to_owned(),
                prompt: prompt.to_owned(),
                images,
                max_tokens,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Some(Batch {
        requests,
        state: calls.get("state").filter(|state| !state.is_null()).cloned(),
    }))
}

static LIMITERS: LazyLock<kovan_map::HopscotchMap<String, Arc<Semaphore>>> =
    LazyLock::new(kovan_map::HopscotchMap::new);

fn limiter(endpoint: &ModelEndpoint) -> Arc<Semaphore> {
    LIMITERS.get_or_insert(
        format!("{}#{}", endpoint.backend_key, endpoint.max_concurrent),
        Arc::new(Semaphore::new(endpoint.max_concurrent)),
    )
}

fn token_limit(asked: Option<u64>, cap: Option<u64>) -> u64 {
    match (asked, cap) {
        (Some(asked), Some(cap)) => asked.min(cap),
        (asked, cap) => asked.or(cap).unwrap_or(DEFAULT_MAX_TOKENS),
    }
}

#[derive(Clone, Copy)]
enum Refusal {
    Dead,
    RequestLimit,
    AnswerBudget,
}

impl Refusal {
    fn message(self) -> &'static str {
        match self {
            Self::Dead => "the model endpoint is not answering; this request was not sent",
            Self::RequestLimit => {
                "this call used up its model request limit; this request was not sent"
            }
            Self::AnswerBudget => {
                "this call used up its model answer budget; this request was not sent"
            }
        }
    }
}

pub struct Session {
    endpoint: ModelEndpoint,
    client: reqwest::Client,
    limiter: Arc<Semaphore>,
    failed_rounds: u32,
    requests_sent: usize,
    answer_bytes: usize,
    max_requests: usize,
    max_answer_bytes: usize,
}

impl Session {
    pub fn new(endpoint: ModelEndpoint) -> Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(endpoint.connect_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("building the model client")?;
        Ok(Self {
            limiter: limiter(&endpoint),
            endpoint,
            client,
            failed_rounds: 0,
            requests_sent: 0,
            answer_bytes: 0,
            max_requests: MAX_REQUESTS_PER_CALL,
            max_answer_bytes: MAX_ANSWER_BYTES_PER_CALL,
        })
    }

    pub async fn serve(&mut self, requests: Vec<Request>, deadline: Instant) -> Map<String, Value> {
        let dead = self.failed_rounds >= DEAD_ENDPOINT_ROUNDS;
        let mut room = self.max_requests.saturating_sub(self.requests_sent);
        let bytes_left = self.answer_bytes < self.max_answer_bytes;
        let plan: Vec<(Request, Option<Refusal>)> = requests
            .into_iter()
            .map(|request| {
                let refusal = if dead {
                    Some(Refusal::Dead)
                } else if room == 0 {
                    Some(Refusal::RequestLimit)
                } else if !bytes_left {
                    Some(Refusal::AnswerBudget)
                } else {
                    room -= 1;
                    None
                };
                (request, refusal)
            })
            .collect();
        let sent = plan.iter().filter(|(_, refusal)| refusal.is_none()).count();
        self.requests_sent += sent;
        let this = &*self;
        let answers: Vec<(String, bool, Result<String, String>)> = futures::stream::iter(plan)
            .map(|(request, refusal)| async move {
                let answer = match refusal {
                    Some(why) => Err(why.message().to_owned()),
                    None => this.ask(&request, deadline).await,
                };
                (request.id, refusal.is_none(), answer)
            })
            .buffer_unordered(MAX_REQUESTS_PER_ROUND)
            .collect()
            .await;
        if sent > 0 {
            if answers
                .iter()
                .all(|(_, was_sent, answer)| !was_sent || answer.is_err())
            {
                self.failed_rounds += 1;
            } else {
                self.failed_rounds = 0;
            }
        }
        let mut results = Map::new();
        for (id, _, answer) in answers {
            let answer = answer.and_then(|text| {
                if self.answer_bytes.saturating_add(text.len()) > self.max_answer_bytes {
                    return Err("this call used up its model answer budget".to_owned());
                }
                self.answer_bytes += text.len();
                Ok(text)
            });
            results.insert(
                id,
                match answer {
                    Ok(text) => json!({"text": text}),
                    Err(error) => json!({"error": error}),
                },
            );
        }
        results
    }

    async fn ask(&self, request: &Request, deadline: Instant) -> Result<String, String> {
        let endpoint = &self.endpoint;
        let mut content = vec![json!({"type": "text", "text": request.prompt})];
        content.extend(request.images.iter().map(|image| {
            json!({"type": "image_url", "image_url": {"url": format!("data:{};base64,{}", image.mime, image.data_base64)}})
        }));
        let mut body = json!({
            "model": endpoint.model,
            "messages": [{"role": "user", "content": content}],
            "temperature": 0,
        });
        body["max_tokens"] = json!(token_limit(request.max_tokens, endpoint.max_output_tokens));
        let body = serde_json::to_vec(&body)
            .map_err(|_| "the model request could not be encoded".to_owned())?;
        let wall = tokio::time::Instant::from_std(deadline);
        let _permit = match tokio::time::timeout_at(wall, self.limiter.acquire()).await {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) => return Err("the model endpoint is not available".to_owned()),
            Err(_) => return Err("the model request timed out".to_owned()),
        };
        let mut runtime_key = None;
        if let Some(visibility) = &endpoint.profile_visibility {
            match tokio::time::timeout_at(wall, check_profile_visibility(visibility)).await {
                Ok(Ok(key)) => {
                    runtime_key = key;
                }
                Ok(Err(_)) => return Err("the selected model profile is unavailable".to_owned()),
                Err(_) => return Err("the model request timed out".to_owned()),
            }
        }
        let mut call = self
            .client
            .post(&endpoint.url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body);
        let private_key = runtime_key.as_deref().or(endpoint.api_key.as_deref());
        if let Some(key) = private_key {
            call = call.bearer_auth(key);
        }
        let exchange = async {
            let response = call.send().await.map_err(|error| {
                if error.is_connect() {
                    "the model endpoint could not be reached".to_owned()
                } else {
                    "the model request failed".to_owned()
                }
            })?;
            let status = response.status();
            if !status.is_success() {
                return Err(format!(
                    "the model endpoint answered HTTP {}",
                    status.as_u16()
                ));
            }
            let mut response = response;
            let mut bytes = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| "the model answer was cut off".to_owned())?
            {
                bytes.extend_from_slice(&chunk);
                if bytes.len() > MAX_RESULT_BYTES * 2 {
                    return Err("the model answer was too large".to_owned());
                }
            }
            let answer: Value = serde_json::from_slice(&bytes)
                .map_err(|_| "the model answer was not JSON".to_owned())?;
            let text = answer_text(&answer).ok_or("the model answer had no text")?;
            if text.len() > MAX_RESULT_BYTES {
                return Err(format!(
                    "the model answer is larger than {MAX_RESULT_BYTES} bytes"
                ));
            }
            Ok(text)
        };
        let limit = (tokio::time::Instant::now() + endpoint.request_timeout).min(wall);
        match tokio::time::timeout_at(limit, exchange).await {
            Ok(result) => result,
            Err(_) => Err("the model request timed out".to_owned()),
        }
    }
}

fn answer_text(answer: &Value) -> Option<String> {
    let content = answer.pointer("/choices/0/message/content")?;
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => Some(
            parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect(),
        ),
        _ => None,
    }
}

#[cfg(test)]
#[path = "model_calls_tests.rs"]
mod tests;
