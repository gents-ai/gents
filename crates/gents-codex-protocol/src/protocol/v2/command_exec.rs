use super::SandboxPolicy;
use rapidhash::RapidHashMap;
use serde::Deserialize;
use serde::Serialize;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecTerminalSize {
    pub rows: u16,
    pub cols: u16,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecParams {
    /// Empty arrays are rejected.
    pub command: Vec<String>,
    /// Optional client-supplied, connection-scoped process id.
    ///
    /// Required for `tty`, `streamStdin`, `streamStdoutStderr`, and follow-up
    /// `command/exec/write`, `command/exec/resize`, and
    /// `command/exec/terminate` calls. When omitted, buffered execution gets an
    /// internal id that is not exposed to the client.
    pub process_id: Option<String>,
    /// PTY mode; implies `streamStdin` and `streamStdoutStderr`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tty: bool,
    /// Allows `command/exec/write` to write stdin bytes; requires a
    /// client-supplied `processId`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stream_stdin: bool,
    /// Stream stdout/stderr via `command/exec/outputDelta`; not duplicated
    /// into the final response and requires a client-supplied `processId`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stream_stdout_stderr: bool,
    /// Per-stream capture cap in bytes; cannot be combined with
    /// `disableOutputCap`. When omitted, the server default applies.
    pub output_bytes_cap: Option<usize>,
    /// Disables stdout/stderr capture truncation; cannot be combined with
    /// `outputBytesCap`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disable_output_cap: bool,
    /// Disables the timeout; cannot be combined with `timeoutMs`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disable_timeout: bool,
    /// Timeout in milliseconds; cannot be combined with `disableTimeout`.
    /// When omitted, the server default applies.
    pub timeout_ms: Option<i64>,
    /// Defaults to the server cwd.
    pub cwd: Option<PathBuf>,
    /// Environment overrides merged into the server-computed environment;
    /// `null` unsets an inherited variable.
    pub env: Option<RapidHashMap<String, Option<String>>>,
    /// Initial PTY size in character cells; only valid when `tty` is true.
    pub size: Option<CommandExecTerminalSize>,
    /// Sandbox policy; cannot be combined with `permissionProfile`. Defaults
    /// to the user's configured policy when omitted.
    pub sandbox_policy: Option<SandboxPolicy>,
    /// Active permissions profile id; cannot be combined with `sandboxPolicy`.
    /// Defaults to the user's configured permissions when omitted.
    pub permission_profile: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecResponse {
    pub exit_code: i32,
    /// Empty when stdout was streamed via `command/exec/outputDelta`.
    pub stdout: String,
    /// Empty when stderr was streamed via `command/exec/outputDelta`.
    pub stderr: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecWriteParams {
    /// Client-supplied, connection-scoped `processId` from the original
    /// `command/exec` request.
    pub process_id: String,
    /// Optional base64-encoded stdin bytes to write.
    pub delta_base64: Option<String>,
    /// Close stdin after writing `deltaBase64`, if present.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub close_stdin: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecWriteResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecTerminateParams {
    /// Client-supplied, connection-scoped `processId` from the original
    /// `command/exec` request.
    pub process_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecTerminateResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecResizeParams {
    /// Client-supplied, connection-scoped `processId` from the original
    /// `command/exec` request.
    pub process_id: String,
    /// New PTY size in character cells.
    pub size: CommandExecTerminalSize,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecResizeResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CommandExecOutputStream {
    /// PTY mode multiplexes terminal output here.
    Stdout,
    Stderr,
}
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecOutputDeltaNotification {
    /// Client-supplied, connection-scoped `processId` from the original
    /// `command/exec` request.
    pub process_id: String,
    /// Output stream for this chunk.
    pub stream: CommandExecOutputStream,
    /// Base64-encoded output bytes.
    pub delta_base64: String,
    /// `true` on the final streamed chunk for a stream when `outputBytesCap`
    /// truncated later output on that stream.
    pub cap_reached: bool,
}
