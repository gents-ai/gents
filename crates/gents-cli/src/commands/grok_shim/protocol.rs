//! Grok shim wire codec: length-prefixed frames and leader envelopes.
//!
//! Gents is the leader server on this socket and stock Grok is the pager
//! client. Both directions speak one JSON value per frame behind a four-byte
//! big-endian `u32` length prefix, capped at [`MAX_MESSAGE_SIZE`] (64MB). A
//! clean end of stream before any header byte is [`ProtocolError::ConnectionClosed`];
//! a header or payload that stops early is a truncation error, never a
//! silently short frame.
//!
//! The envelope vocabulary is the leader wire protocol, not ACP itself:
//! `register` / `registered` (with [`LEADER_PROTOCOL_VERSION`] and the
//! `gents-<version>` [`LEADER_BINARY_VERSION`]), `leader_ready` gating,
//! `ping` / `pong`, `acp` pass-through (one raw JSON-RPC 2.0 line per frame),
//! `control`, `shutting_down` + `shutdown`, and `error`. ACP payloads are
//! never parsed by the frame layer beyond the JSON-RPC 2.0 id/payload types
//! in this module; the shim imports no Grok code.
//!
//! Internal agent methods are wire-named with a leading underscore so a
//! decoder routes them to ext method/notification handling:
//! `_x.ai/internal/auth_cleared`, `_x.ai/internal/evict_sessions`, ...
//! (see [`INTERNAL_AGENT_METHODS`] and [`internal_method_wire_name`]).

use std::borrow::Cow;
use std::fmt;

use serde::de::{self, DeserializeOwned, Deserializer};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Largest frame the codec will read or write: 64MB, matching the Grok leader
/// protocol's `MAX_MESSAGE_SIZE`.
pub const MAX_MESSAGE_SIZE: usize = 64 * 1024 * 1024;

/// Length-prefix width in bytes (big-endian `u32`).
pub const FRAME_PREFIX_BYTES: usize = 4;

/// Leader protocol version spoken by this shim.
pub const LEADER_PROTOCOL_VERSION: u32 = 1;

/// Leader binary version reported in `registered`, verbatim from the wire
/// contract: `format!("gents-{}", env!("CARGO_PKG_VERSION"))`.
pub const LEADER_BINARY_VERSION: &str = concat!("gents-", env!("CARGO_PKG_VERSION"));

/// JSON-RPC 2.0 protocol tag.
pub const JSONRPC_VERSION: &str = "2.0";

/// JSON-RPC 2.0 error codes used by the shim's shaped responses.
pub const JSONRPC_PARSE_ERROR: i64 = -32700;
pub const JSONRPC_INVALID_REQUEST: i64 = -32600;
pub const JSONRPC_METHOD_NOT_FOUND: i64 = -32601;
pub const JSONRPC_INVALID_PARAMS: i64 = -32602;
pub const JSONRPC_INTERNAL_ERROR: i64 = -32603;

/// Leading underscore the leader wire protocol puts on internal agent
/// methods so decoders route them to ext method/notification handling.
pub const INTERNAL_WIRE_PREFIX: char = '_';

/// Internal agent method names (without the wire prefix). Wire form is
/// [`internal_method_wire_name`]: `_x.ai/internal/auth_cleared`, etc.
pub const INTERNAL_AGENT_METHODS: [&str; 8] = [
    "x.ai/internal/auth_cleared",
    "x.ai/internal/evict_sessions",
    "x.ai/internal/reload_all_mcp_servers",
    "x.ai/internal/reload_models",
    "x.ai/internal/reload_models_cache",
    "x.ai/internal/reload_project_mcp_servers",
    "x.ai/internal/reload_skills",
    "x.ai/internal/reload_workflows",
];

// ---------------------------------------------------------------------------
// Frame errors
// ---------------------------------------------------------------------------

/// Errors produced by the frame codec and envelope decoding.
#[derive(Debug)]
pub enum ProtocolError {
    /// A frame declared a length above [`MAX_MESSAGE_SIZE`].
    MessageTooLarge(usize),
    /// The stream ended before any prefix byte: a clean disconnect.
    ConnectionClosed,
    /// The stream ended partway through the four-byte length prefix.
    TruncatedHeader { read: usize },
    /// The stream ended before the declared payload length was reached.
    TruncatedFrame { declared: usize, read: usize },
    /// A frame body was not a decodable value of the expected shape.
    InvalidFrame(String),
    /// The underlying stream failed.
    Io(std::io::Error),
}

impl ProtocolError {
    /// True when the peer disconnected cleanly before any header byte.
    pub fn is_connection_closed(&self) -> bool {
        matches!(self, ProtocolError::ConnectionClosed)
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProtocolError::MessageTooLarge(len) => {
                write!(
                    f,
                    "frame length {len} exceeds the {MAX_MESSAGE_SIZE} byte limit"
                )
            }
            ProtocolError::ConnectionClosed => {
                write!(f, "connection closed before a frame prefix was read")
            }
            ProtocolError::TruncatedHeader { read } => {
                write!(
                    f,
                    "connection closed after {read} of {FRAME_PREFIX_BYTES} prefix bytes"
                )
            }
            ProtocolError::TruncatedFrame { declared, read } => write!(
                f,
                "connection closed after {read} of {declared} declared frame bytes"
            ),
            ProtocolError::InvalidFrame(detail) => {
                write!(f, "frame body is not a decodable value: {detail}")
            }
            ProtocolError::Io(error) => write!(f, "frame transport failed: {error}"),
        }
    }
}

impl std::error::Error for ProtocolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ProtocolError::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ProtocolError {
    fn from(error: std::io::Error) -> Self {
        ProtocolError::Io(error)
    }
}

impl From<serde_json::Error> for ProtocolError {
    fn from(error: serde_json::Error) -> Self {
        ProtocolError::InvalidFrame(error.to_string())
    }
}

// ---------------------------------------------------------------------------
// Frame IO
// ---------------------------------------------------------------------------

/// Read one length-prefixed frame and return its payload bytes.
///
/// Returns [`ProtocolError::ConnectionClosed`] only for a clean end of stream
/// before the first prefix byte; a prefix or body that stops early is a
/// truncation error. Declared lengths above [`MAX_MESSAGE_SIZE`] are rejected
/// before any allocation, so a hostile peer cannot force a giant buffer.
pub async fn read_frame<R>(reader: &mut R) -> Result<Vec<u8>, ProtocolError>
where
    R: AsyncRead + Unpin,
{
    let mut prefix = [0u8; FRAME_PREFIX_BYTES];
    let prefixed = read_full(reader, &mut prefix).await?;
    if prefixed == 0 {
        return Err(ProtocolError::ConnectionClosed);
    }
    if prefixed < FRAME_PREFIX_BYTES {
        return Err(ProtocolError::TruncatedHeader { read: prefixed });
    }
    let declared = u32::from_be_bytes(prefix) as usize;
    if declared > MAX_MESSAGE_SIZE {
        tracing::trace!(declared, "grok shim frame rejected as oversized");
        return Err(ProtocolError::MessageTooLarge(declared));
    }

    let mut payload = vec![0u8; declared];
    let read = read_full(reader, &mut payload).await?;
    if read < declared {
        return Err(ProtocolError::TruncatedFrame { declared, read });
    }
    tracing::trace!(declared, "grok shim frame read");
    Ok(payload)
}

/// Write one length-prefixed frame.
///
/// The payload is flushed before returning so a peer never observes a partial
/// frame body. Payloads above [`MAX_MESSAGE_SIZE`] are rejected up front.
pub async fn write_frame<W>(writer: &mut W, payload: &[u8]) -> Result<(), ProtocolError>
where
    W: AsyncWrite + Unpin,
{
    if payload.len() > u32::MAX as usize || payload.len() > MAX_MESSAGE_SIZE {
        return Err(ProtocolError::MessageTooLarge(payload.len()));
    }
    let prefix = (payload.len() as u32).to_be_bytes();
    writer.write_all(&prefix).await?;
    writer.write_all(payload).await?;
    writer.flush().await?;
    tracing::trace!(len = payload.len(), "grok shim frame written");
    Ok(())
}

/// Read one frame and decode it as `T`.
pub async fn read_frame_as<T, R>(reader: &mut R) -> Result<T, ProtocolError>
where
    T: DeserializeOwned,
    R: AsyncRead + Unpin,
{
    let payload = read_frame(reader).await?;
    decode_frame(&payload)
}

/// Encode `value` and write it as one frame.
pub async fn write_frame_as<T, W>(writer: &mut W, value: &T) -> Result<(), ProtocolError>
where
    T: Serialize,
    W: AsyncWrite + Unpin,
{
    let payload = serde_json::to_vec(value)?;
    write_frame(writer, &payload).await
}

/// Decode a frame body as `T`.
pub fn decode_frame<T>(payload: &[u8]) -> Result<T, ProtocolError>
where
    T: DeserializeOwned,
{
    serde_json::from_slice(payload).map_err(ProtocolError::from)
}

/// Encode a value into frame body bytes.
pub fn encode_frame<T>(value: &T) -> Result<Vec<u8>, ProtocolError>
where
    T: Serialize,
{
    serde_json::to_vec(value).map_err(ProtocolError::from)
}

/// Read one client (pager → leader) envelope.
pub async fn read_client_envelope<R>(reader: &mut R) -> Result<ClientEnvelope, ProtocolError>
where
    R: AsyncRead + Unpin,
{
    read_frame_as(reader).await
}

/// Read one server (leader → pager) envelope.
pub async fn read_server_envelope<R>(reader: &mut R) -> Result<ServerEnvelope, ProtocolError>
where
    R: AsyncRead + Unpin,
{
    read_frame_as(reader).await
}

/// Write one client (pager → leader) envelope.
pub async fn write_client_envelope<W>(
    writer: &mut W,
    envelope: &ClientEnvelope,
) -> Result<(), ProtocolError>
where
    W: AsyncWrite + Unpin,
{
    write_frame_as(writer, envelope).await
}

/// Write one server (leader → pager) envelope.
pub async fn write_server_envelope<W>(
    writer: &mut W,
    envelope: &ServerEnvelope,
) -> Result<(), ProtocolError>
where
    W: AsyncWrite + Unpin,
{
    write_frame_as(writer, envelope).await
}

/// Fill `buf` as far as the stream allows, tolerating `Interrupted`.
///
/// Returns the number of bytes read; a return below `buf.len()` means the
/// stream ended.
async fn read_full<R>(reader: &mut R, buf: &mut [u8]) -> Result<usize, ProtocolError>
where
    R: AsyncRead + Unpin,
{
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]).await {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(ProtocolError::Io(error)),
        }
    }
    Ok(filled)
}

// ---------------------------------------------------------------------------
// Client (pager → leader) envelopes
// ---------------------------------------------------------------------------

/// Capabilities a registering client may advertise. Every flag is optional on
/// the wire; unknown keys are ignored so newer pagers still register.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ClientCapabilities {
    /// Approve-all tool execution; the leader injects `yoloMode: true`.
    #[serde(default)]
    pub yolo_mode: bool,
    /// Classifier-driven auto approval; the leader injects `autoMode: true`.
    #[serde(default)]
    pub auto_mode: bool,
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub client_version: Option<String>,
    #[serde(default)]
    pub code_nav_enabled: bool,
    /// Route terminal commands to the client; the reference pager is `false`.
    #[serde(default)]
    pub terminal: bool,
    #[serde(default)]
    pub fs_read: bool,
    #[serde(default)]
    pub fs_write: bool,
    #[serde(default)]
    pub status_line: bool,
}

impl ClientCapabilities {
    /// True when the registering client asked for approve-all execution.
    pub fn is_always_approve(&self) -> bool {
        self.yolo_mode
    }

    /// True when the registering client asked for classifier auto approval.
    pub fn is_auto(&self) -> bool {
        self.auto_mode
    }
}

/// Registration mode: interactive stdio pager or headless client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegisterMode {
    Stdio,
    Headless,
}

impl RegisterMode {
    pub fn wire_name(self) -> &'static str {
        match self {
            RegisterMode::Stdio => "stdio",
            RegisterMode::Headless => "headless",
        }
    }
}

/// Envelope sent by the pager client to the Gents leader.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientEnvelope {
    /// First envelope on a fresh connection; the leader answers `registered`.
    Register {
        client_type: String,
        mode: RegisterMode,
        #[serde(default)]
        capabilities: ClientCapabilities,
    },
    /// One raw JSON-RPC 2.0 line passed through to/from the ACP service.
    Acp { payload: String },
    /// Control command addressed by request id.
    Control { request_id: String, command: Value },
    /// Liveness probe; answered with `pong`.
    Ping,
    /// Graceful client disconnect.
    Disconnect,
}

impl ClientEnvelope {
    /// Build an `acp` pass-through envelope from any JSON-RPC payload.
    pub fn acp<T>(payload: &T) -> Result<Self, ProtocolError>
    where
        T: Serialize,
    {
        Ok(ClientEnvelope::Acp {
            payload: encode_acp_payload(payload)?,
        })
    }

    /// The raw JSON-RPC payload, for `acp` envelopes only.
    pub fn acp_payload(&self) -> Option<&str> {
        match self {
            ClientEnvelope::Acp { payload } => Some(payload.as_str()),
            _ => None,
        }
    }

    /// The registering client's capabilities, for `register` envelopes only.
    pub fn capabilities(&self) -> Option<&ClientCapabilities> {
        match self {
            ClientEnvelope::Register { capabilities, .. } => Some(capabilities),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Server (leader → pager) envelopes
// ---------------------------------------------------------------------------

/// Capabilities the leader advertises in `registered`.
///
/// The v1 shim only advertises what it genuinely answers, so every flag
/// defaults to false and an empty profile-format list; assembly flips a flag
/// on only when the corresponding surface is wired.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LeaderCapabilities {
    #[serde(default)]
    pub control_v1: bool,
    #[serde(default)]
    pub runtime_cpu_profile: bool,
    #[serde(default)]
    pub profile_formats: Vec<String>,
    #[serde(default)]
    pub workspace_exposure: bool,
    #[serde(default)]
    pub relaunch_v1: bool,
}

impl LeaderCapabilities {
    /// Profile formats the v1 shim supports: none, since CPU profiling is
    /// not wired. Kept explicit so `registered` always carries the key.
    pub fn profile_formats(&self) -> &[String] {
        &self.profile_formats
    }
}

/// Why the leader is shutting down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShutdownReason {
    AutoUpdate,
    Manual,
    IdleTimeout,
}

impl ShutdownReason {
    pub fn wire_name(self) -> &'static str {
        match self {
            ShutdownReason::AutoUpdate => "auto_update",
            ShutdownReason::Manual => "manual",
            ShutdownReason::IdleTimeout => "idle_timeout",
        }
    }
}

fn default_ready() -> bool {
    true
}

fn default_leader_protocol_version() -> u32 {
    LEADER_PROTOCOL_VERSION
}

fn default_leader_binary_version() -> String {
    LEADER_BINARY_VERSION.to_string()
}

/// Envelope sent by the Gents leader to the pager client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEnvelope {
    /// Answer to `register`. `ready: false` means the client must wait for
    /// [`ServerEnvelope::LeaderReady`] before sending any ACP traffic.
    Registered {
        client_id: u64,
        #[serde(default = "default_ready")]
        ready: bool,
        #[serde(default = "default_leader_protocol_version")]
        leader_protocol_version: u32,
        #[serde(default = "default_leader_binary_version")]
        leader_binary_version: String,
        #[serde(default)]
        leader_capabilities: LeaderCapabilities,
    },
    /// Leader finished initializing; ACP traffic may now flow.
    LeaderReady,
    /// One raw JSON-RPC 2.0 line passed through to/from the ACP service.
    Acp { payload: String },
    /// Answer to `ping`.
    Pong,
    /// Protocol-level error.
    Error { code: i32, message: String },
    /// Announced before `shutdown`, with the delay before it lands.
    ShuttingDown {
        reason: ShutdownReason,
        #[serde(default)]
        delay_ms: u64,
    },
    /// Terminal: the leader is going away.
    Shutdown,
}

impl ServerEnvelope {
    /// Build the `registered` answer for a client id and readiness flag,
    /// stamping the shim's protocol and binary versions.
    pub fn registered(client_id: u64, ready: bool) -> Self {
        ServerEnvelope::Registered {
            client_id,
            ready,
            leader_protocol_version: LEADER_PROTOCOL_VERSION,
            leader_binary_version: LEADER_BINARY_VERSION.to_string(),
            leader_capabilities: LeaderCapabilities::default(),
        }
    }

    /// Build an `acp` pass-through envelope from any JSON-RPC payload.
    pub fn acp<T>(payload: &T) -> Result<Self, ProtocolError>
    where
        T: Serialize,
    {
        Ok(ServerEnvelope::Acp {
            payload: encode_acp_payload(payload)?,
        })
    }

    /// Build a protocol-level `error` envelope.
    pub fn error(code: i32, message: impl Into<String>) -> Self {
        ServerEnvelope::Error {
            code,
            message: message.into(),
        }
    }

    /// Build the `shutting_down` announcement that precedes `shutdown`.
    pub fn shutting_down(reason: ShutdownReason, delay_ms: u64) -> Self {
        ServerEnvelope::ShuttingDown { reason, delay_ms }
    }

    /// The raw JSON-RPC payload, for `acp` envelopes only.
    pub fn acp_payload(&self) -> Option<&str> {
        match self {
            ServerEnvelope::Acp { payload } => Some(payload.as_str()),
            _ => None,
        }
    }

    /// The client id, for `registered` envelopes only.
    pub fn client_id(&self) -> Option<u64> {
        match self {
            ServerEnvelope::Registered { client_id, .. } => Some(*client_id),
            _ => None,
        }
    }

    /// Whether this envelope permits ACP traffic: only `registered` with
    /// `ready: true`, or an already-issued [`ServerEnvelope::LeaderReady`]
    /// (tracked by the caller), does.
    pub fn is_registered_ready(&self) -> bool {
        matches!(
            self,
            ServerEnvelope::Registered { ready: true, .. } | ServerEnvelope::LeaderReady
        )
    }
}

/// Encode a JSON-RPC payload into the string carried by an `acp` envelope.
pub fn encode_acp_payload<T>(payload: &T) -> Result<String, ProtocolError>
where
    T: Serialize,
{
    serde_json::to_string(payload).map_err(ProtocolError::from)
}

/// Decode the string carried by an `acp` envelope into a JSON-RPC payload.
pub fn decode_acp_payload<T>(payload: &str) -> Result<T, ProtocolError>
where
    T: DeserializeOwned,
{
    serde_json::from_str(payload).map_err(ProtocolError::from)
}

// ---------------------------------------------------------------------------
// Internal agent method routing
// ---------------------------------------------------------------------------

/// Wire name of an internal agent method: a leading underscore so a decoder
/// routes it to ext method/notification handling.
pub fn internal_method_wire_name(method: &str) -> String {
    format!("{INTERNAL_WIRE_PREFIX}{method}")
}

/// Strip the internal wire prefix, if present.
pub fn strip_internal_wire_prefix(wire_method: &str) -> Option<&str> {
    wire_method.strip_prefix(INTERNAL_WIRE_PREFIX)
}

/// True when the method carries the internal wire prefix.
pub fn is_internal_wire_method(wire_method: &str) -> bool {
    wire_method.starts_with(INTERNAL_WIRE_PREFIX)
}

/// True when the wire method is one of the known internal agent methods
/// (underscore-prefixed on the wire, e.g. `_x.ai/internal/reload_skills`).
pub fn is_known_internal_agent_method(wire_method: &str) -> bool {
    match strip_internal_wire_prefix(wire_method) {
        Some(name) => INTERNAL_AGENT_METHODS.contains(&name),
        None => false,
    }
}

// ---------------------------------------------------------------------------
// JSON-RPC 2.0 ids and payloads
// ---------------------------------------------------------------------------

/// A JSON-RPC 2.0 id: a string, an integer, or null (used only when echoing a
/// peer's null id). Other shapes are rejected at decode time.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum JsonRpcId {
    Number(i64),
    String(String),
    Null,
}

impl JsonRpcId {
    /// Render the id as a JSON value.
    pub fn to_value(&self) -> Value {
        match self {
            JsonRpcId::Number(number) => Value::from(*number),
            JsonRpcId::String(text) => Value::from(text.as_str()),
            JsonRpcId::Null => Value::Null,
        }
    }
}

impl fmt::Display for JsonRpcId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JsonRpcId::Number(number) => write!(f, "{number}"),
            JsonRpcId::String(text) => write!(f, "{text}"),
            JsonRpcId::Null => write!(f, "null"),
        }
    }
}

impl Serialize for JsonRpcId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            JsonRpcId::Number(number) => serializer.serialize_i64(*number),
            JsonRpcId::String(text) => serializer.serialize_str(text),
            JsonRpcId::Null => serializer.serialize_unit(),
        }
    }
}

impl<'de> Deserialize<'de> for JsonRpcId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        match value {
            Value::Null => Ok(JsonRpcId::Null),
            Value::Number(number) => {
                if let Some(number) = number.as_i64() {
                    return Ok(JsonRpcId::Number(number));
                }
                if let Some(number) = number.as_u64() {
                    if let Ok(number) = i64::try_from(number) {
                        return Ok(JsonRpcId::Number(number));
                    }
                }
                Err(<D::Error as de::Error>::custom(format!(
                    "invalid JSON-RPC id: {number}"
                )))
            }
            Value::String(text) => Ok(JsonRpcId::String(text)),
            other => Err(<D::Error as de::Error>::custom(format!(
                "invalid JSON-RPC id: {other}"
            ))),
        }
    }
}

fn default_jsonrpc_version() -> String {
    JSONRPC_VERSION.to_string()
}

/// A JSON-RPC 2.0 request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcRequest<P = Value> {
    #[serde(default = "default_jsonrpc_version")]
    pub jsonrpc: String,
    pub id: JsonRpcId,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<P>,
}

impl<P> JsonRpcRequest<P> {
    pub fn new(id: JsonRpcId, method: impl Into<String>, params: P) -> Self {
        JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            method: method.into(),
            params: Some(params),
        }
    }
}

impl JsonRpcRequest<Value> {
    /// The request params, or JSON null when the peer omitted them.
    pub fn params_or_null(&self) -> &Value {
        static NULL: Value = Value::Null;
        self.params.as_ref().unwrap_or(&NULL)
    }

    /// The request params as an object, or an empty object when absent.
    pub fn params_object(&self) -> Cow<'_, serde_json::Map<String, Value>> {
        match self.params.as_ref() {
            Some(Value::Object(map)) => Cow::Borrowed(map),
            _ => Cow::Owned(serde_json::Map::new()),
        }
    }
}

/// A JSON-RPC 2.0 notification (no id, never answered).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcNotification<P = Value> {
    #[serde(default = "default_jsonrpc_version")]
    pub jsonrpc: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<P>,
}

impl<P> JsonRpcNotification<P> {
    pub fn new(method: impl Into<String>, params: P) -> Self {
        JsonRpcNotification {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: method.into(),
            params: Some(params),
        }
    }
}

/// A successful JSON-RPC 2.0 response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcResponse<P = Value> {
    #[serde(default = "default_jsonrpc_version")]
    pub jsonrpc: String,
    pub id: JsonRpcId,
    pub result: P,
}

impl<P> JsonRpcResponse<P> {
    pub fn new(id: JsonRpcId, result: P) -> Self {
        JsonRpcResponse {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result,
        }
    }
}

/// The `error` object of a failed JSON-RPC 2.0 response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        RpcError {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// The shaped method-not-found error used for unsupported agent methods.
    pub fn method_not_found(method: &str, explanation: &str) -> Self {
        RpcError::new(
            JSONRPC_METHOD_NOT_FOUND,
            format!("method not found: {method}: {explanation}"),
        )
    }
}

/// A failed JSON-RPC 2.0 response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcErrorResponse {
    #[serde(default = "default_jsonrpc_version")]
    pub jsonrpc: String,
    pub id: JsonRpcId,
    pub error: RpcError,
}

impl JsonRpcErrorResponse {
    pub fn new(id: JsonRpcId, error: RpcError) -> Self {
        JsonRpcErrorResponse {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            error,
        }
    }

    /// Shaped method-not-found response for an unsupported agent method.
    pub fn method_not_found(id: JsonRpcId, method: &str, explanation: &str) -> Self {
        JsonRpcErrorResponse::new(id, RpcError::method_not_found(method, explanation))
    }

    /// Shaped invalid-params response.
    pub fn invalid_params(id: JsonRpcId, message: impl Into<String>) -> Self {
        JsonRpcErrorResponse::new(id, RpcError::new(JSONRPC_INVALID_PARAMS, message))
    }

    /// Shaped internal-error response.
    pub fn internal_error(id: JsonRpcId, message: impl Into<String>) -> Self {
        JsonRpcErrorResponse::new(id, RpcError::new(JSONRPC_INTERNAL_ERROR, message))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Round-trip one value through a real duplex stream using the frame
    /// codec, so serialization, prefixing, and decoding are all exercised.
    async fn round_trip<E>(envelope: E) -> E
    where
        E: Serialize + DeserializeOwned,
    {
        let (mut writer, mut reader) = tokio::io::duplex(8192);
        write_frame_as(&mut writer, &envelope).await.unwrap();
        read_frame_as::<E, _>(&mut reader).await.unwrap()
    }

    fn sample_client_envelopes() -> Vec<ClientEnvelope> {
        vec![
            ClientEnvelope::Register {
                client_type: "pager".to_string(),
                mode: RegisterMode::Stdio,
                capabilities: ClientCapabilities {
                    yolo_mode: true,
                    auto_mode: false,
                    default_model: Some("GLM-5.3-NVFP4".to_string()),
                    client_version: Some("grok 1.0".to_string()),
                    code_nav_enabled: true,
                    terminal: false,
                    fs_read: true,
                    fs_write: true,
                    status_line: false,
                },
            },
            ClientEnvelope::Register {
                client_type: "headless".to_string(),
                mode: RegisterMode::Headless,
                capabilities: ClientCapabilities::default(),
            },
            ClientEnvelope::Acp {
                payload: r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#.to_string(),
            },
            ClientEnvelope::Control {
                request_id: "control-1".to_string(),
                command: json!({ "op": "relaunch" }),
            },
            ClientEnvelope::Ping,
            ClientEnvelope::Disconnect,
        ]
    }

    fn sample_server_envelopes() -> Vec<ServerEnvelope> {
        vec![
            ServerEnvelope::registered(7, true),
            ServerEnvelope::Registered {
                client_id: 9,
                ready: false,
                leader_protocol_version: LEADER_PROTOCOL_VERSION,
                leader_binary_version: LEADER_BINARY_VERSION.to_string(),
                leader_capabilities: LeaderCapabilities {
                    control_v1: true,
                    runtime_cpu_profile: false,
                    profile_formats: vec!["svg".to_string(), "folded".to_string()],
                    workspace_exposure: false,
                    relaunch_v1: false,
                },
            },
            ServerEnvelope::LeaderReady,
            ServerEnvelope::Acp {
                payload: r#"{"jsonrpc":"2.0","id":1,"result":{"sessionId":"s"}}"#.to_string(),
            },
            ServerEnvelope::Pong,
            ServerEnvelope::Error {
                code: -1,
                message: "boom".to_string(),
            },
            ServerEnvelope::shutting_down(ShutdownReason::IdleTimeout, 0),
            ServerEnvelope::Shutdown,
        ]
    }

    #[tokio::test]
    async fn client_envelopes_round_trip_through_frames() {
        for envelope in sample_client_envelopes() {
            let decoded = round_trip(envelope.clone()).await;
            assert_eq!(decoded, envelope);
        }
    }

    #[tokio::test]
    async fn server_envelopes_round_trip_through_frames() {
        for envelope in sample_server_envelopes() {
            let decoded = round_trip(envelope.clone()).await;
            assert_eq!(decoded, envelope);
        }
    }

    #[tokio::test]
    async fn consecutive_frames_keep_order() {
        let (mut writer, mut reader) = tokio::io::duplex(8192);
        write_client_envelope(&mut writer, &ClientEnvelope::Ping)
            .await
            .unwrap();
        write_client_envelope(&mut writer, &ClientEnvelope::Disconnect)
            .await
            .unwrap();
        assert_eq!(
            read_client_envelope(&mut reader).await.unwrap(),
            ClientEnvelope::Ping
        );
        assert_eq!(
            read_client_envelope(&mut reader).await.unwrap(),
            ClientEnvelope::Disconnect
        );
    }

    #[test]
    fn register_frame_matches_grok_wire_shape() {
        let raw = br#"{"type":"register","client_type":"pager","mode":"stdio","capabilities":{"yolo_mode":true,"auto_mode":false,"default_model":"GLM-5.3-NVFP4","client_version":"grok 1.2.3","code_nav_enabled":true,"terminal":false,"fs_read":true,"fs_write":true,"status_line":false}}"#;
        let envelope = decode_frame::<ClientEnvelope>(raw).unwrap();
        let ClientEnvelope::Register {
            client_type,
            mode,
            capabilities,
        } = envelope
        else {
            panic!("expected a register envelope");
        };
        assert_eq!(client_type, "pager");
        assert_eq!(mode.wire_name(), "stdio");
        assert!(capabilities.is_always_approve());
        assert!(!capabilities.is_auto());
        assert_eq!(capabilities.default_model.as_deref(), Some("GLM-5.3-NVFP4"));
        assert!(!capabilities.terminal);

        let encoded = serde_json::to_value(&envelope).unwrap();
        assert_eq!(encoded.pointer("/type"), Some(&json!("register")));
        assert_eq!(encoded.pointer("/client_type"), Some(&json!("pager")));
        assert_eq!(encoded.pointer("/mode"), Some(&json!("stdio")));
        assert_eq!(
            encoded.pointer("/capabilities/yolo_mode"),
            Some(&json!(true))
        );
    }

    #[test]
    fn register_accepts_headless_mode_and_unknown_keys() {
        let raw = br#"{"type":"register","client_type":"headless","mode":"headless","capabilities":{"future_flag":true}}"#;
        let envelope = decode_frame::<ClientEnvelope>(raw).unwrap();
        let ClientEnvelope::Register {
            mode, capabilities, ..
        } = envelope
        else {
            panic!("expected a register envelope");
        };
        assert_eq!(mode.wire_name(), "headless");
        assert_eq!(capabilities, ClientCapabilities::default());
    }

    #[test]
    fn register_without_capabilities_defaults_to_empty() {
        let raw = br#"{"type":"register","client_type":"pager","mode":"stdio"}"#;
        let envelope = decode_frame::<ClientEnvelope>(raw).unwrap();
        assert_eq!(
            envelope.capabilities(),
            Some(&ClientCapabilities::default())
        );
    }

    #[test]
    fn registered_frame_matches_grok_wire_shape() {
        let envelope = ServerEnvelope::registered(42, true);
        let encoded = serde_json::to_value(&envelope).unwrap();
        assert_eq!(encoded.pointer("/type"), Some(&json!("registered")));
        assert_eq!(encoded.pointer("/client_id"), Some(&json!(42)));
        assert_eq!(encoded.pointer("/ready"), Some(&json!(true)));
        assert_eq!(
            encoded.pointer("/leader_protocol_version"),
            Some(&json!(LEADER_PROTOCOL_VERSION))
        );
        assert_eq!(
            encoded.pointer("/leader_binary_version"),
            Some(&json!(LEADER_BINARY_VERSION))
        );
        assert_eq!(
            encoded.pointer("/leader_capabilities/control_v1"),
            Some(&json!(false))
        );
        assert!(envelope.client_id() == Some(42));
        assert!(envelope.is_registered_ready());

        // `ready` defaults to true when a peer omits it.
        let decoded =
            decode_frame::<ServerEnvelope>(br#"{"type":"registered","client_id":1}"#).unwrap();
        assert!(decoded.is_registered_ready());

        let not_ready = ServerEnvelope::Registered {
            client_id: 1,
            ready: false,
            leader_protocol_version: LEADER_PROTOCOL_VERSION,
            leader_binary_version: LEADER_BINARY_VERSION.to_string(),
            leader_capabilities: LeaderCapabilities::default(),
        };
        assert!(!not_ready.is_registered_ready());
        // A leader_ready envelope re-opens the gate.
        assert!(ServerEnvelope::LeaderReady.is_registered_ready());
    }

    #[test]
    fn leader_binary_version_is_gents_prefixed() {
        assert_eq!(
            LEADER_BINARY_VERSION,
            format!("gents-{}", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn ping_pong_acp_control_and_shutdown_wire_names() {
        assert_eq!(
            serde_json::to_value(ClientEnvelope::Ping).unwrap(),
            json!({ "type": "ping" })
        );
        assert_eq!(
            serde_json::to_value(ClientEnvelope::Disconnect).unwrap(),
            json!({ "type": "disconnect" })
        );
        assert_eq!(
            serde_json::to_value(ServerEnvelope::Pong).unwrap(),
            json!({ "type": "pong" })
        );
        assert_eq!(
            serde_json::to_value(ServerEnvelope::LeaderReady).unwrap(),
            json!({ "type": "leader_ready" })
        );
        assert_eq!(
            serde_json::to_value(ServerEnvelope::Shutdown).unwrap(),
            json!({ "type": "shutdown" })
        );

        let control = ClientEnvelope::Control {
            request_id: "req-1".to_string(),
            command: json!({ "op": "relaunch" }),
        };
        assert_eq!(
            serde_json::to_value(&control).unwrap(),
            json!({ "type": "control", "request_id": "req-1", "command": { "op": "relaunch" } })
        );

        let acp = ClientEnvelope::Acp {
            payload: "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}".to_string(),
        };
        assert_eq!(
            serde_json::to_value(&acp).unwrap(),
            json!({
                "type": "acp",
                "payload": "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}"
            })
        );
        assert!(acp.acp_payload().is_some());
        assert!(acp.capabilities().is_none());

        let shutting_down = ServerEnvelope::shutting_down(ShutdownReason::Manual, 250);
        assert_eq!(
            serde_json::to_value(&shutting_down).unwrap(),
            json!({ "type": "shutting_down", "reason": "manual", "delay_ms": 250 })
        );
        assert_eq!(ShutdownReason::AutoUpdate.wire_name(), "auto_update");
        assert_eq!(ShutdownReason::IdleTimeout.wire_name(), "idle_timeout");

        let error = ServerEnvelope::error(-32000, "nope");
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            json!({ "type": "error", "code": -32000, "message": "nope" })
        );
    }

    #[tokio::test]
    async fn clean_eof_is_connection_closed() {
        let (_writer, mut reader) = tokio::io::duplex(64);
        drop(_writer);
        let error = read_frame(&mut reader).await.unwrap_err();
        assert!(error.is_connection_closed());
        assert!(matches!(error, ProtocolError::ConnectionClosed));
    }

    #[tokio::test]
    async fn partial_prefix_is_truncated_header() {
        let (mut writer, mut reader) = tokio::io::duplex(64);
        writer.write_all(&[0, 0]).await.unwrap();
        drop(writer);
        let error = read_frame(&mut reader).await.unwrap_err();
        assert!(matches!(error, ProtocolError::TruncatedHeader { read: 2 }));
    }

    #[tokio::test]
    async fn short_payload_is_truncated_frame() {
        let (mut writer, mut reader) = tokio::io::duplex(64);
        let prefix = 10u32.to_be_bytes();
        writer.write_all(&prefix).await.unwrap();
        writer.write_all(b"abcd").await.unwrap();
        drop(writer);
        let error = read_frame(&mut reader).await.unwrap_err();
        assert!(matches!(
            error,
            ProtocolError::TruncatedFrame {
                declared: 10,
                read: 4
            }
        ));
    }

    #[tokio::test]
    async fn oversize_frame_is_rejected_before_allocation() {
        let (mut writer, mut reader) = tokio::io::duplex(64);
        let declared = (MAX_MESSAGE_SIZE + 1) as u32;
        writer.write_all(&declared.to_be_bytes()).await.unwrap();
        drop(writer);
        let error = read_frame(&mut reader).await.unwrap_err();
        assert!(matches!(
            error,
            ProtocolError::MessageTooLarge(size) if size == MAX_MESSAGE_SIZE + 1
        ));
    }

    #[tokio::test]
    async fn invalid_json_body_is_invalid_frame() {
        let (mut writer, mut reader) = tokio::io::duplex(64);
        write_frame(&mut writer, b"{not json").await.unwrap();
        let error = read_client_envelope(&mut reader).await.unwrap_err();
        match error {
            ProtocolError::InvalidFrame(detail) => {
                assert!(!detail.is_empty());
            }
            other => panic!("expected an invalid frame error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn zero_length_frame_is_invalid_frame() {
        let (mut writer, mut reader) = tokio::io::duplex(64);
        write_frame(&mut writer, &[]).await.unwrap();
        let error = read_client_envelope(&mut reader).await.unwrap_err();
        assert!(matches!(error, ProtocolError::InvalidFrame(_)));
    }

    #[tokio::test]
    async fn wrong_envelope_kind_is_invalid_frame() {
        // A client envelope on the server side must not decode as a server
        // envelope: the `type` tags are disjoint.
        let (mut writer, mut reader) = tokio::io::duplex(64);
        write_client_envelope(&mut writer, &ClientEnvelope::Ping)
            .await
            .unwrap();
        let error = read_server_envelope(&mut reader).await.unwrap_err();
        assert!(matches!(error, ProtocolError::InvalidFrame(_)));
    }

    #[test]
    fn jsonrpc_id_accepts_number_string_and_null() {
        assert_eq!(
            serde_json::from_value::<JsonRpcId>(json!(17)).unwrap(),
            JsonRpcId::Number(17)
        );
        assert_eq!(
            serde_json::from_value::<JsonRpcId>(json!("abc")).unwrap(),
            JsonRpcId::String("abc".to_string())
        );
        assert_eq!(
            serde_json::from_value::<JsonRpcId>(Value::Null).unwrap(),
            JsonRpcId::Null
        );
        assert_eq!(
            serde_json::to_value(JsonRpcId::Number(17)).unwrap(),
            json!(17)
        );
        assert_eq!(
            serde_json::to_value(JsonRpcId::String("abc".to_string())).unwrap(),
            json!("abc")
        );
        assert_eq!(serde_json::to_value(JsonRpcId::Null).unwrap(), Value::Null);
        assert_eq!(JsonRpcId::Number(17).to_string(), "17");
        assert_eq!(JsonRpcId::String("x".into()).to_value(), json!("x"));
    }

    #[test]
    fn jsonrpc_id_rejects_other_shapes() {
        assert!(serde_json::from_value::<JsonRpcId>(json!(true)).is_err());
        assert!(serde_json::from_value::<JsonRpcId>(json!([1])).is_err());
        assert!(serde_json::from_value::<JsonRpcId>(json!({})).is_err());
        assert!(serde_json::from_value::<JsonRpcId>(json!(1.5)).is_err());
    }

    #[test]
    fn jsonrpc_request_and_response_shapes() {
        let request = JsonRpcRequest::new(
            JsonRpcId::Number(3),
            "session/prompt",
            json!({ "sessionId": "s1", "prompt": [] }),
        );
        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(encoded.pointer("/jsonrpc"), Some(&json!("2.0")));
        assert_eq!(encoded.pointer("/id"), Some(&json!(3)));
        assert_eq!(encoded.pointer("/method"), Some(&json!("session/prompt")));
        assert_eq!(encoded.pointer("/params/sessionId"), Some(&json!("s1")));
        assert_eq!(
            request.params_or_null().pointer("/sessionId"),
            Some(&json!("s1"))
        );
        assert!(request.params_object().contains_key("sessionId"));

        // A request without params decodes with `None`.
        let bare =
            decode_frame::<JsonRpcRequest<Value>>(br#"{"jsonrpc":"2.0","id":"x","method":"ping"}"#)
                .unwrap();
        assert_eq!(bare.method, "ping");
        assert!(bare.params.is_none());
        assert!(bare.params_or_null().is_null());
        assert!(bare.params_object().is_empty());

        let response =
            JsonRpcResponse::new(JsonRpcId::Number(3), json!({ "stopReason": "end_turn" }));
        let encoded = serde_json::to_value(&response).unwrap();
        assert_eq!(encoded.pointer("/id"), Some(&json!(3)));
        assert_eq!(
            encoded.pointer("/result/stopReason"),
            Some(&json!("end_turn"))
        );
    }

    #[test]
    fn jsonrpc_notification_has_no_id() {
        let notification = JsonRpcNotification::new(
            "session/cancel",
            json!({ "sessionId": "s1", "_meta": { "cancelSubagents": true } }),
        );
        let encoded = serde_json::to_value(&notification).unwrap();
        assert!(encoded.get("id").is_none());
        assert_eq!(encoded.pointer("/method"), Some(&json!("session/cancel")));
        assert_eq!(
            encoded.pointer("/params/_meta/cancelSubagents"),
            Some(&json!(true))
        );

        let bare = decode_frame::<JsonRpcNotification<Value>>(
            br#"{"jsonrpc":"2.0","method":"session/cancel"}"#,
        )
        .unwrap();
        assert_eq!(bare.method, "session/cancel");
        assert!(bare.params.is_none());
    }

    #[test]
    fn method_not_found_error_is_shaped() {
        let response = JsonRpcErrorResponse::method_not_found(
            JsonRpcId::String("req-9".to_string()),
            "session/load",
            "the Gents shim owns session creation and never replays persisted updates",
        );
        assert_eq!(response.error.code, JSONRPC_METHOD_NOT_FOUND);
        assert!(response.error.message.contains("session/load"));
        assert!(response
            .error
            .message
            .contains("the Gents shim owns session creation"));
        let encoded = serde_json::to_value(&response).unwrap();
        assert_eq!(encoded.pointer("/jsonrpc"), Some(&json!("2.0")));
        assert_eq!(encoded.pointer("/id"), Some(&json!("req-9")));
        assert_eq!(encoded.pointer("/error/code"), Some(&json!(-32601)));
        assert!(encoded.get("result").is_none());
        assert!(encoded.pointer("/error/data").is_none());

        let invalid_params =
            JsonRpcErrorResponse::invalid_params(JsonRpcId::Number(1), "missing sessionId");
        assert_eq!(invalid_params.error.code, JSONRPC_INVALID_PARAMS);
        let internal = JsonRpcErrorResponse::internal_error(JsonRpcId::Number(1), "boom");
        assert_eq!(internal.error.code, JSONRPC_INTERNAL_ERROR);
    }

    #[test]
    fn internal_agent_methods_are_underscore_prefixed() {
        assert_eq!(
            internal_method_wire_name("x.ai/internal/reload_skills"),
            "_x.ai/internal/reload_skills"
        );
        assert_eq!(
            strip_internal_wire_prefix("_x.ai/internal/reload_skills"),
            Some("x.ai/internal/reload_skills")
        );
        assert_eq!(strip_internal_wire_prefix("session/prompt"), None);
        assert!(is_internal_wire_method("_x.ai/internal/auth_cleared"));
        assert!(!is_internal_wire_method("x.ai/interject"));
        for name in INTERNAL_AGENT_METHODS {
            let wire = internal_method_wire_name(name);
            assert!(is_known_internal_agent_method(&wire), "{wire}");
        }
        // Unknown underscore methods are still internal-shaped, but not one
        // of the audited agent methods.
        assert!(is_internal_wire_method("_x.ai/internal/not_a_real_method"));
        assert!(!is_known_internal_agent_method(
            "_x.ai/internal/not_a_real_method"
        ));
        // Plain ACP methods are never internal.
        for method in [
            "initialize",
            "session/new",
            "session/prompt",
            "session/cancel",
            "x.ai/interject",
            "x.ai/compact_conversation",
        ] {
            assert!(!is_internal_wire_method(method));
            assert!(!is_known_internal_agent_method(method));
        }
    }

    #[tokio::test]
    async fn acp_envelope_helpers_encode_and_pass_through() {
        let request = JsonRpcRequest::new(
            JsonRpcId::Number(11),
            "session/new",
            json!({ "cwd": "/tmp", "mcpServers": [] }),
        );
        let client_envelope = ClientEnvelope::acp(&request).unwrap();
        assert_eq!(
            client_envelope.acp_payload(),
            Some(serde_json::to_string(&request).unwrap().as_str())
        );

        let server_envelope = ServerEnvelope::acp(&request).unwrap();
        assert_eq!(server_envelope.acp_payload(), client_envelope.acp_payload());

        let decoded: JsonRpcRequest<Value> =
            decode_acp_payload(server_envelope.acp_payload().unwrap()).unwrap();
        assert_eq!(decoded, request);

        // Full pass-through over a real stream, both directions.
        let (mut writer, mut reader) = tokio::io::duplex(8192);
        write_client_envelope(&mut writer, &client_envelope)
            .await
            .unwrap();
        assert_eq!(
            read_client_envelope(&mut reader).await.unwrap(),
            client_envelope
        );
        write_server_envelope(&mut writer, &server_envelope)
            .await
            .unwrap();
        assert_eq!(
            read_server_envelope(&mut reader).await.unwrap(),
            server_envelope
        );
    }

    #[test]
    fn frame_encode_decode_helpers_round_trip() {
        let envelope = ServerEnvelope::registered(3, true);
        let bytes = encode_frame(&envelope).unwrap();
        let prefix = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        assert_eq!(prefix as usize, bytes.len() - FRAME_PREFIX_BYTES);
        let decoded: ServerEnvelope = decode_frame(&bytes[4..]).unwrap();
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn protocol_error_display_is_actionable() {
        assert!(ProtocolError::MessageTooLarge(MAX_MESSAGE_SIZE + 1)
            .to_string()
            .contains("exceeds"));
        assert!(ProtocolError::ConnectionClosed
            .to_string()
            .contains("closed"));
        assert!(ProtocolError::TruncatedHeader { read: 2 }
            .to_string()
            .contains("2 of 4"));
        assert!(ProtocolError::TruncatedFrame {
            declared: 10,
            read: 4
        }
        .to_string()
        .contains("4 of 10"));
        assert!(ProtocolError::InvalidFrame("bad".to_string())
            .to_string()
            .contains("bad"));
        let io_error = ProtocolError::from(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "broken pipe",
        ));
        assert!(io_error.to_string().contains("broken pipe"));
        let json_error = serde_json::from_str::<Value>("{").unwrap_err();
        assert!(ProtocolError::from(json_error)
            .to_string()
            .contains("decodable"));
    }
}
