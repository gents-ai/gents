use std::fmt;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Reject oversized peer frames before allocating their declared payload.
pub const MAX_MESSAGE_SIZE: usize = 64 * 1024 * 1024;

/// The wire length prefix is a big-endian `u32` on both read and write.
pub const FRAME_PREFIX_BYTES: usize = 4;

pub const LEADER_PROTOCOL_VERSION: u32 = 1;

pub const LEADER_BINARY_VERSION: &str = concat!("gents-", env!("CARGO_PKG_VERSION"));

#[derive(Debug)]
pub enum ProtocolError {
    MessageTooLarge(usize),
    ConnectionClosed,
    TruncatedHeader { read: usize },
    TruncatedFrame { declared: usize, read: usize },
    InvalidFrame(String),
    Io(std::io::Error),
}

impl ProtocolError {
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

pub async fn read_frame_as<T, R>(reader: &mut R) -> Result<T, ProtocolError>
where
    T: DeserializeOwned,
    R: AsyncRead + Unpin,
{
    let payload = read_frame(reader).await?;
    decode_frame(&payload)
}

pub async fn write_frame_as<T, W>(writer: &mut W, value: &T) -> Result<(), ProtocolError>
where
    T: Serialize,
    W: AsyncWrite + Unpin,
{
    let payload = serde_json::to_vec(value)?;
    write_frame(writer, &payload).await
}

pub fn decode_frame<T>(payload: &[u8]) -> Result<T, ProtocolError>
where
    T: DeserializeOwned,
{
    serde_json::from_slice(payload).map_err(ProtocolError::from)
}

pub async fn read_client_envelope<R>(reader: &mut R) -> Result<ClientEnvelope, ProtocolError>
where
    R: AsyncRead + Unpin,
{
    read_frame_as(reader).await
}

#[cfg(test)]
pub async fn read_server_envelope<R>(reader: &mut R) -> Result<ServerEnvelope, ProtocolError>
where
    R: AsyncRead + Unpin,
{
    read_frame_as(reader).await
}

#[cfg(test)]
pub async fn write_client_envelope<W>(
    writer: &mut W,
    envelope: &ClientEnvelope,
) -> Result<(), ProtocolError>
where
    W: AsyncWrite + Unpin,
{
    write_frame_as(writer, envelope).await
}

pub async fn write_server_envelope<W>(
    writer: &mut W,
    envelope: &ServerEnvelope,
) -> Result<(), ProtocolError>
where
    W: AsyncWrite + Unpin,
{
    write_frame_as(writer, envelope).await
}

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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ClientCapabilities {
    #[serde(default)]
    pub yolo_mode: bool,
    #[serde(default)]
    pub auto_mode: bool,
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub client_version: Option<String>,
    #[serde(default)]
    pub code_nav_enabled: bool,
    #[serde(default)]
    pub terminal: bool,
    #[serde(default)]
    pub fs_read: bool,
    #[serde(default)]
    pub fs_write: bool,
    #[serde(default)]
    pub status_line: bool,
}

#[cfg(test)]
impl ClientCapabilities {
    pub fn is_always_approve(&self) -> bool {
        self.yolo_mode
    }

    pub fn is_auto(&self) -> bool {
        self.auto_mode
    }
}

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientEnvelope {
    Register {
        client_type: String,
        mode: RegisterMode,
        #[serde(default)]
        capabilities: ClientCapabilities,
    },
    Acp {
        payload: String,
    },
    Control {
        request_id: String,
        command: Value,
    },
    Ping,
    Disconnect,
}

#[cfg(test)]
impl ClientEnvelope {
    pub fn acp_payload(&self) -> Option<&str> {
        match self {
            ClientEnvelope::Acp { payload } => Some(payload.as_str()),
            _ => None,
        }
    }

    pub fn capabilities(&self) -> Option<&ClientCapabilities> {
        match self {
            ClientEnvelope::Register { capabilities, .. } => Some(capabilities),
            _ => None,
        }
    }
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShutdownReason {
    AutoUpdate,
    Manual,
    IdleTimeout,
}

#[cfg(test)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEnvelope {
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
    LeaderReady,
    Acp {
        payload: String,
    },
    Pong,
    Error {
        code: i32,
        message: String,
    },
    ShuttingDown {
        reason: ShutdownReason,
        #[serde(default)]
        delay_ms: u64,
    },
    Shutdown,
}

#[cfg(test)]
impl ServerEnvelope {
    pub fn registered(client_id: u64, ready: bool) -> Self {
        ServerEnvelope::Registered {
            client_id,
            ready,
            leader_protocol_version: LEADER_PROTOCOL_VERSION,
            leader_binary_version: LEADER_BINARY_VERSION.to_string(),
            leader_capabilities: LeaderCapabilities::default(),
        }
    }

    pub fn error(code: i32, message: impl Into<String>) -> Self {
        ServerEnvelope::Error {
            code,
            message: message.into(),
        }
    }

    pub fn shutting_down(reason: ShutdownReason, delay_ms: u64) -> Self {
        ServerEnvelope::ShuttingDown { reason, delay_ms }
    }

    pub fn client_id(&self) -> Option<u64> {
        match self {
            ServerEnvelope::Registered { client_id, .. } => Some(*client_id),
            _ => None,
        }
    }

    pub fn is_registered_ready(&self) -> bool {
        matches!(
            self,
            ServerEnvelope::Registered { ready: true, .. } | ServerEnvelope::LeaderReady
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
            ref client_type,
            mode,
            ref capabilities,
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
        assert!(ServerEnvelope::LeaderReady.is_registered_ready());
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
        let (mut writer, mut reader) = tokio::io::duplex(64);
        write_client_envelope(&mut writer, &ClientEnvelope::Ping)
            .await
            .unwrap();
        let error = read_server_envelope(&mut reader).await.unwrap_err();
        assert!(matches!(error, ProtocolError::InvalidFrame(_)));
    }
}
