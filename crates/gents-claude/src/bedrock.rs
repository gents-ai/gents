//! The Amazon Bedrock runtime rules of Claude Code 2.1.311 that are not shared
//! with the other platforms: the request path with the SDK's model escaping,
//! the body rewrite, the region-prefix rewrite of model ids and the choice of an
//! inference profile from the account's list, the `ListInferenceProfiles`
//! response, and the mapping of event-stream frames to the Messages SSE events.
//!
//! Everything here is a pure function over bytes and strings. The body rewrite
//! is a function over the planned body (`wire::to_bytes` output), not a second
//! body builder: it removes `model` and `stream`, places `anthropic_beta`, and
//! appends `anthropic_version`, as the SDK middleware (`chunk-956fdk9f.js`) and
//! Claude Code's `kK` do.

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::eventstream::Frame;
use crate::wire::{RawObject, WireError};

/// The `anthropic_version` Bedrock takes in the body.
pub const ANTHROPIC_VERSION: &str = "bedrock-2023-05-31";

/// The streaming content type, which the response must carry or omit.
pub const EVENT_STREAM_CONTENT_TYPE: &str = "application/vnd.amazon.eventstream";

/// Cross-region inference profile prefixes Claude Code recognizes (`gJ`).
const PROFILE_PREFIXES: [&str; 7] = ["us", "eu", "apac", "jp", "au", "us-gov", "global"];

/// Why a Bedrock request or response could not be built or read.
#[derive(Debug, thiserror::Error)]
pub enum BedrockError {
    /// The planned body is not a JSON object.
    #[error("the planned Messages body cannot be rewritten for Bedrock: {0}")]
    Body(#[from] WireError),
    /// The planned body is not UTF-8.
    #[error("the planned Messages body is not valid UTF-8")]
    BodyNotUtf8,
    /// The model id cannot be a single path segment.
    #[error("the model id {0:?} cannot be used in a Bedrock request path")]
    ModelId(String),
    /// A `ListInferenceProfiles` response is not the expected JSON.
    #[error("the inference profile list is not valid: {0}")]
    ProfileList(#[source] serde_json::Error),
}

/// The SDK's path-template escaping: every run of characters outside
/// `A-Za-z0-9-._~!$&'()*+,;=:@` is `encodeURIComponent`ed, so `:` stays and `/`
/// becomes `%2F`.
fn escape_path_segment(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 8);
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'.'
                    | b'_'
                    | b'~'
                    | b'!'
                    | b'$'
                    | b'&'
                    | b'\''
                    | b'('
                    | b')'
                    | b'*'
                    | b'+'
                    | b','
                    | b';'
                    | b'='
                    | b':'
                    | b'@'
            )
        {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The streaming invoke path of `model`: `/model/<id>/invoke-with-response-stream`.
pub fn invoke_path(model: &str) -> Result<String, BedrockError> {
    // The SDK refuses a dot segment as a path parameter; the escaping never
    // produces `%2e`, so only the literal forms can occur.
    if matches!(model, "" | "." | "..") {
        return Err(BedrockError::ModelId(model.to_owned()));
    }
    Ok(format!(
        "/model/{}/invoke-with-response-stream",
        escape_path_segment(model)
    ))
}

fn raw_string_array(values: &[&str]) -> Result<Box<RawValue>, serde_json::Error> {
    RawValue::from_string(serde_json::to_string(values)?)
}

/// Rewrites a planned Messages body for Bedrock.
///
/// `body_betas` are the betas Claude Code moves into the body (`cXo`, plus the
/// thinking betas `Tve` appends); they sit where the extra-body spread lands,
/// before `output_config`. `header_betas` are the `anthropic-beta` header
/// values: when the body has no `anthropic_beta` the SDK copies them there,
/// after `anthropic_version`. `model` and `stream` are removed.
pub fn transform_body(
    body: &[u8],
    body_betas: &[&str],
    header_betas: &[&str],
) -> Result<Vec<u8>, BedrockError> {
    let text = std::str::from_utf8(body).map_err(|_| BedrockError::BodyNotUtf8)?;
    let mut object = RawObject::from_json(text)?;
    let wire = |error| BedrockError::Body(WireError::Serialize(error));
    if !body_betas.is_empty() {
        object.insert_before(
            &["output_config", "speed", "thread", "diagnostics", "stream"],
            "anthropic_beta",
            raw_string_array(body_betas).map_err(wire)?,
        );
    }
    if !object.contains("anthropic_version") {
        object.push(
            "anthropic_version",
            RawValue::from_string(format!("\"{ANTHROPIC_VERSION}\"")).map_err(wire)?,
        );
    }
    if !object.contains("anthropic_beta") && !header_betas.is_empty() {
        object.push(
            "anthropic_beta",
            raw_string_array(header_betas).map_err(wire)?,
        );
    }
    object.remove("model");
    object.remove("stream");
    serde_json::to_vec(&object).map_err(wire)
}

/// The inference profile prefix of a region (`cqt` without the environment
/// override): `us-gov`, `us`, `eu`, `apac`, else `global`.
pub fn region_prefix(region: &str) -> &'static str {
    if region.starts_with("us-gov-") {
        "us-gov"
    } else if region.starts_with("us-") {
        "us"
    } else if region.starts_with("eu-") {
        "eu"
    } else if region.starts_with("ap-") {
        "apac"
    } else {
        "global"
    }
}

/// Rewrites the geo prefix of a catalog Bedrock model id (`o$e`): a recognized
/// `<prefix>.anthropic.` id takes `prefix`, a bare `anthropic.` id gains it,
/// anything else is unchanged.
pub fn rewrite_model_id(id: &str, prefix: &str) -> String {
    let name = if id.starts_with("arn:") {
        id.rfind('/').map_or(id, |at| &id[at + 1..])
    } else {
        id
    };
    for known in PROFILE_PREFIXES {
        if name.starts_with(&format!("{known}.anthropic.")) {
            return id.replacen(&format!("{known}."), &format!("{prefix}."), 1);
        }
    }
    if id.starts_with("anthropic.") {
        return format!("{prefix}.{id}");
    }
    id.to_owned()
}

/// Whether `rest` (the text after a model name in a profile id) starts a
/// version or date suffix rather than a longer model name (`Nh`).
fn continues_version(rest: &str) -> bool {
    let bytes = rest.as_bytes();
    match bytes.first() {
        Some(b'0'..=b'9') => true,
        Some(b'-') => {
            let digits = bytes[1..].iter().take_while(|b| b.is_ascii_digit()).count();
            (1..=2).contains(&digits)
        }
        _ => false,
    }
}

/// Whether the profile id `profile` names `first_party` and not a longer name
/// such as `claude-sonnet-4-5` for `claude-sonnet-4` (`Nh`).
fn names_model(profile: &str, first_party: &str) -> bool {
    let mut from = 0;
    while let Some(found) = profile[from..].find(first_party) {
        let start = from + found;
        if !continues_version(&profile[start + first_party.len()..]) {
            return true;
        }
        // Advance one character past the match start.
        from = start + profile[start..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// The listed profile for the first-party id `first_party` (`rTt`): one with
/// the wanted prefix first, else any that names the model.
pub fn select_profile<'a>(
    profiles: &'a [String],
    first_party: &str,
    prefix: &str,
) -> Option<&'a str> {
    let dotted = format!("{prefix}.");
    profiles
        .iter()
        .find(|id| id.starts_with(&dotted) && names_model(id, first_party))
        .or_else(|| profiles.iter().find(|id| names_model(id, first_party)))
        .map(String::as_str)
}

/// One page of `ListInferenceProfiles`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfilePage {
    /// Anthropic profile ids on the page.
    pub ids: Vec<String>,
    /// The token of the next page, absent on the last.
    pub next_token: Option<String>,
}

/// Parses a `ListInferenceProfiles` response, keeping the ids that contain `anthropic`.
pub fn parse_profile_page(body: &[u8]) -> Result<ProfilePage, BedrockError> {
    #[derive(Deserialize)]
    struct Summary {
        #[serde(rename = "inferenceProfileId", default)]
        id: Option<String>,
    }
    #[derive(Deserialize)]
    struct Page {
        #[serde(rename = "inferenceProfileSummaries", default)]
        summaries: Vec<Summary>,
        #[serde(rename = "nextToken", default)]
        next_token: Option<String>,
    }
    let page: Page = serde_json::from_slice(body).map_err(BedrockError::ProfileList)?;
    Ok(ProfilePage {
        ids: page
            .summaries
            .into_iter()
            .filter_map(|summary| summary.id)
            .filter(|id| id.contains("anthropic"))
            .collect(),
        next_token: page.next_token.filter(|token| !token.is_empty()),
    })
}

/// `[a-z]{2,4}(?:-[a-z]{2,5})?-[a-z]+-\d+`: an AWS region as the host table spells it.
fn region_label(label: &str) -> bool {
    let parts: Vec<&str> = label.split('-').collect();
    let alpha = |part: &str, min: usize, max: usize| {
        (min..=max).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_lowercase())
    };
    let tail_ok = |zone: &str, number: &str| {
        !zone.is_empty()
            && zone.bytes().all(|b| b.is_ascii_lowercase())
            && !number.is_empty()
            && number.bytes().all(|b| b.is_ascii_digit())
    };
    match parts.as_slice() {
        [geo, zone, number] => alpha(geo, 2, 4) && tail_ok(zone, number),
        [geo, sub, zone, number] => alpha(geo, 2, 4) && alpha(sub, 2, 5) && tail_ok(zone, number),
        _ => false,
    }
}

/// Whether `endpoint` is an own Bedrock runtime host outside GovCloud
/// (`Acr`): https, `bedrock-runtime[-fips].<region>.amazonaws.com|api.aws`,
/// optionally behind a `vpce-` label. Eager tool input streaming applies only
/// there.
pub fn is_own_runtime_endpoint(endpoint: &str) -> bool {
    let Ok(url) = url::Url::parse(endpoint) else {
        return false;
    };
    if url.scheme() != "https" {
        return false;
    }
    let host = url
        .host_str()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .trim_end_matches('.')
        .to_owned();
    let mut labels: Vec<&str> = host.split('.').collect();
    if labels
        .first()
        .is_some_and(|first| first.starts_with("vpce-"))
    {
        labels.remove(0);
    }
    let [service, region, rest @ ..] = labels.as_slice() else {
        return false;
    };
    matches!(*service, "bedrock-runtime" | "bedrock-runtime-fips")
        && region_label(region)
        && !region.starts_with("us-gov-")
        && matches!(
            rest,
            ["amazonaws", "com"]
                | ["api", "aws"]
                | ["vpce", "amazonaws", "com"]
                | ["vpce", "api", "aws"]
        )
}

/// What one decoded frame means to the Messages stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mapped {
    /// A Messages SSE event to feed the parser.
    Event(String),
    /// An error SSE event; the stream ends after it.
    Error(String),
    /// A frame that carries no Messages event.
    Ignored,
}

/// Why a frame cannot become a Messages event.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StreamError {
    /// A chunk event holds no valid JSON.
    #[error("a Bedrock chunk could not be read as JSON: {0}")]
    Chunk(String),
    /// An exception frame's payload is not the expected JSON.
    #[error("a Bedrock exception frame could not be read: {0}")]
    Exception(String),
    /// A frame is neither an event, an exception nor a named error.
    #[error("an unrecognized Bedrock stream frame of kind {0:?}")]
    Unrecognized(String),
}

/// Exceptions the runtime API models (`:exception-type` values).
const MODELED_EXCEPTIONS: [&str; 6] = [
    "internalServerException",
    "modelStreamErrorException",
    "validationException",
    "throttlingException",
    "modelTimeoutException",
    "serviceUnavailableException",
];

#[derive(Serialize)]
struct ErrorBody<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    message: &'a str,
}

#[derive(Serialize)]
struct ErrorEvent<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    error: ErrorBody<'a>,
}

fn error_sse(kind: &str, message: &str) -> String {
    let event = ErrorEvent {
        kind: "error",
        error: ErrorBody { kind, message },
    };
    // Two strings in a fixed shape always serialize.
    let data = serde_json::to_string(&event).unwrap_or_default();
    format!("event: error\ndata: {data}\n\n")
}

fn lower_first(name: &str) -> String {
    let mut chars = name.chars();
    chars
        .next()
        .map(|first| first.to_lowercase().chain(chars).collect())
        .unwrap_or_default()
}

/// Maps a frame as the SDK's stream reader does: a `chunk` event holding
/// `{"bytes": base64}` becomes `event: <type>` with the decoded JSON, a modeled
/// exception becomes `{type: lowerFirst(name), message}`, and a named error
/// frame becomes `{type: error-code, message: error-message}`.
pub fn map_frame(frame: &Frame) -> Result<Mapped, StreamError> {
    match frame.string_header(":message-type") {
        Some("event") => map_event(frame),
        Some("exception") => map_exception(frame),
        Some("error") => {
            let code = frame
                .string_header(":error-code")
                .filter(|code| !code.is_empty() && *code != "Error")
                .ok_or_else(|| StreamError::Unrecognized("error".to_owned()))?;
            let message = frame
                .string_header(":error-message")
                .filter(|message| !message.is_empty())
                .unwrap_or("UnknownError");
            Ok(Mapped::Error(error_sse(code, message)))
        }
        other => Err(StreamError::Unrecognized(other.unwrap_or("").to_owned())),
    }
}

fn map_event(frame: &Frame) -> Result<Mapped, StreamError> {
    if frame.string_header(":event-type") != Some("chunk") {
        return Ok(Mapped::Ignored);
    }
    #[derive(Deserialize)]
    struct Chunk {
        #[serde(default)]
        bytes: Option<String>,
    }
    let chunk: Chunk = serde_json::from_slice(&frame.payload)
        .map_err(|error| StreamError::Chunk(error.to_string()))?;
    let Some(encoded) = chunk.bytes.filter(|bytes| !bytes.is_empty()) else {
        return Ok(Mapped::Ignored);
    };
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .map_err(|error| StreamError::Chunk(error.to_string()))?;
    let text =
        std::str::from_utf8(&decoded).map_err(|error| StreamError::Chunk(error.to_string()))?;
    // `JSON.stringify(JSON.parse(text))`, key order kept, as the SDK re-emits it.
    let data =
        crate::wire::js_restringify(text).map_err(|error| StreamError::Chunk(error.to_string()))?;
    #[derive(Deserialize)]
    struct Typed {
        #[serde(rename = "type")]
        kind: Option<serde_json::Value>,
    }
    let typed: Typed =
        serde_json::from_str(data.get()).map_err(|error| StreamError::Chunk(error.to_string()))?;
    let Some(kind) = typed.kind.as_ref().and_then(serde_json::Value::as_str) else {
        return Ok(Mapped::Ignored);
    };
    Ok(Mapped::Event(format!(
        "event: {kind}\ndata: {}\n\n",
        data.get()
    )))
}

fn map_exception(frame: &Frame) -> Result<Mapped, StreamError> {
    let name = frame
        .string_header(":exception-type")
        .ok_or_else(|| StreamError::Unrecognized("exception".to_owned()))?;
    if !MODELED_EXCEPTIONS.contains(&name) {
        // An unmodeled exception carries its payload text as the message.
        let message = String::from_utf8_lossy(&frame.payload);
        return Ok(Mapped::Error(error_sse(name, &message)));
    }
    #[derive(Deserialize)]
    struct Payload {
        #[serde(default)]
        message: Option<String>,
    }
    let payload: Payload = serde_json::from_slice(&frame.payload)
        .map_err(|error| StreamError::Exception(error.to_string()))?;
    Ok(Mapped::Error(error_sse(
        &lower_first(name),
        payload.message.as_deref().unwrap_or(""),
    )))
}

#[cfg(test)]
#[path = "bedrock_tests.rs"]
mod tests;
