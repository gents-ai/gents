//! The HTTP header set of a Messages request in the client's insertion order
//! (SDK `buildHeaders` in `chunk-wcddya4j.js`, app defaults `Pl`).
//!
//! [`request_headers`] returns the headers exactly as the client's JS names
//! them, in the order they sit after the SDK merge: a later layer that sets an
//! existing name deletes the earlier entry and appends its own, so
//! `User-Agent` lands after `x-app`, not after `Accept`. The layers are the
//! SDK base (`Accept`, retry count, timeout, platform, browser-access flag,
//! `anthropic-version`), the auth header, the app defaults (`x-app`,
//! `User-Agent`, session id, then the agent and request-class hints), the body
//! `content-type`, and the per-request `anthropic-beta` and
//! `x-client-request-id`. Wire casing and on-the-wire order are finally decided
//! by Bun's native fetch and are settled at the transport layer; this module
//! owns the logical set and its order.
//!
//! Not reproduced because Gents never sets them: `ANTHROPIC_CUSTOM_HEADERS`,
//! the remote container, remote session and session-origin headers,
//! `x-anthropic-additional-protection`, `traceparent` and the compaction stamp
//! headers.

use std::fmt;

use crate::profile;

/// `anthropic-dangerous-direct-browser-access` is sent because the client sets `dangerouslyAllowBrowser`.
const BROWSER_ACCESS: &str = "anthropic-dangerous-direct-browser-access";

/// A header value, flagged when it carries a credential.
#[derive(Clone, PartialEq, Eq)]
pub struct HeaderValue {
    /// The value to put on the wire.
    pub value: String,
    /// The value is a credential: never log it, mark it sensitive in the HTTP client.
    pub sensitive: bool,
}

impl fmt::Debug for HeaderValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.sensitive {
            f.write_str("HeaderValue(<redacted>)")
        } else {
            write!(f, "HeaderValue({:?})", self.value)
        }
    }
}

/// How the request authenticates.
#[derive(Clone, Copy)]
pub enum Auth<'a> {
    /// Subscriber OAuth token, sent as `Authorization: Bearer`.
    Bearer(&'a str),
    /// API key, sent as `X-Api-Key`.
    ApiKey(&'a str),
}

impl fmt::Debug for Auth<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Auth::Bearer(_) => "Auth::Bearer(<redacted>)",
            Auth::ApiKey(_) => "Auth::ApiKey(<redacted>)",
        })
    }
}

/// `x-claude-code-agent-type` of a user-defined agent (an `agent:custom` query source).
pub const CUSTOM_AGENT_TYPE: &str = "custom";

/// The identity of a subagent making the request.
#[derive(Debug, Clone, Copy)]
pub struct AgentHeaders<'a> {
    /// The subagent id, sent as `x-claude-code-agent-id`.
    pub agent_id: &'a str,
    /// The spawning agent's id, sent as `x-claude-code-parent-agent-id`.
    pub parent_agent_id: Option<&'a str>,
}

/// What a request is for, sent as `x-claude-code-request-class`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestClass {
    /// The main conversation thread.
    Main,
    /// A subagent.
    Subagent,
    /// A workflow agent.
    Workflow,
    /// Conversation compaction.
    Compaction,
    /// Any other helper query.
    Auxiliary,
}

impl RequestClass {
    /// The wire value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Subagent => "subagent",
            Self::Workflow => "workflow",
            Self::Compaction => "compaction",
            Self::Auxiliary => "auxiliary",
        }
    }
}

/// Everything that decides the header set of one request.
#[derive(Debug, Clone, Copy)]
pub struct HeaderContext<'a> {
    /// Client entrypoint, such as `cli` or `sdk-ts`.
    pub entrypoint: &'a str,
    /// `CLAUDE_AGENT_SDK_VERSION`, when running under the Agent SDK.
    pub agent_sdk_version: Option<&'a str>,
    /// `CLAUDE_AGENT_SDK_CLIENT_APP`, sent in the user agent and as `x-client-app`.
    pub client_app: Option<&'a str>,
    /// Workload tag of the request, such as `cron`.
    pub workload: Option<&'a str>,
    /// The session is a background session (`x-app: cli-bg`).
    pub background: bool,
    /// Session id, sent as `X-Claude-Code-Session-Id`.
    pub session_id: &'a str,
    /// `X-Stainless-OS` value, see [`platform_os`].
    pub os: &'a str,
    /// `X-Stainless-Arch` value, see [`platform_arch`].
    pub arch: &'a str,
    /// Credential of the request.
    pub auth: Auth<'a>,
    /// First-party API on its default base URL, which enables the hint headers
    /// and `x-client-request-id`.
    pub first_party_base_url: bool,
    /// Subagent identity, when the request is made by one.
    pub agent: Option<AgentHeaders<'a>>,
    /// Agent type, for `agent:` query sources.
    pub agent_type: Option<&'a str>,
    /// Class of the request.
    pub request_class: RequestClass,
    /// Prompt id; sent only when it is a UUID.
    pub prompt_id: Option<&'a str>,
    /// Beta names, joined by commas in `anthropic-beta`.
    pub betas: &'a [&'a str],
    /// Id of this request, sent as `x-client-request-id`.
    pub client_request_id: &'a str,
    /// SDK retry attempt, sent as `X-Stainless-Retry-Count`.
    pub retry_count: u32,
    /// Request timeout; sent in whole seconds as `X-Stainless-Timeout` when non-zero.
    pub timeout_ms: u64,
}

/// The `User-Agent` of the client (`rF`).
pub fn user_agent(
    entrypoint: &str,
    agent_sdk_version: Option<&str>,
    client_app: Option<&str>,
    workload: Option<&str>,
) -> String {
    let part = |prefix: &str, value: Option<&str>| match value {
        Some(value) if !value.is_empty() => format!(", {prefix}/{value}"),
        _ => String::new(),
    };
    format!(
        "claude-cli/{} (external, {entrypoint}{}{}{})",
        profile::VERSION,
        part("agent-sdk", agent_sdk_version),
        part("client-app", client_app),
        part("workload", workload),
    )
}

/// The SDK's `X-Stainless-OS` for a Node `process.platform` string.
pub fn platform_os(platform: &str) -> String {
    let lower = platform.to_lowercase();
    match lower.as_str() {
        _ if lower.contains("ios") => "iOS".into(),
        "android" => "Android".into(),
        "darwin" => "MacOS".into(),
        "win32" => "Windows".into(),
        "freebsd" => "FreeBSD".into(),
        "openbsd" => "OpenBSD".into(),
        "linux" => "Linux".into(),
        "" => "Unknown".into(),
        _ => format!("Other:{lower}"),
    }
}

/// The SDK's `X-Stainless-Arch` for a Node `process.arch` string.
pub fn platform_arch(arch: &str) -> String {
    match arch {
        "x32" => "x32".into(),
        "x86_64" | "x64" => "x64".into(),
        "arm" => "arm".into(),
        "aarch64" | "arm64" => "arm64".into(),
        "" => "unknown".into(),
        _ => format!("other:{arch}"),
    }
}

/// This host's Node-style `(process.platform, process.arch)`.
pub fn host_platform() -> (&'static str, &'static str) {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        other => other,
    };
    (os, arch)
}

/// Percent-encodes `%` and every character outside printable ASCII (`uXr`).
fn escape_agent_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if c != '%' && (' '..='~').contains(&c) {
            out.push(c);
        } else {
            let mut buf = [0u8; 4];
            for byte in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    out
}

/// Whether `value` is a 8-4-4-4-12 hex UUID of any version, ignoring case.
fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

/// The request headers in the client's final insertion order.
pub fn request_headers(ctx: &HeaderContext<'_>) -> Vec<(&'static str, HeaderValue)> {
    let mut headers = Vec::with_capacity(28);
    let mut push = |name: &'static str, value: String| {
        headers.push((
            name,
            HeaderValue {
                value,
                sensitive: false,
            },
        ))
    };

    push("Accept", "application/json".into());
    push("X-Stainless-Retry-Count", ctx.retry_count.to_string());
    if ctx.timeout_ms > 0 {
        push("X-Stainless-Timeout", (ctx.timeout_ms / 1000).to_string());
    }
    push("X-Stainless-Lang", "js".into());
    push("X-Stainless-Package-Version", profile::SDK_VERSION.into());
    push("X-Stainless-OS", ctx.os.into());
    push("X-Stainless-Arch", ctx.arch.into());
    push("X-Stainless-Runtime", "node".into());
    push(
        "X-Stainless-Runtime-Version",
        profile::RUNTIME_VERSION.into(),
    );
    push(BROWSER_ACCESS, "true".into());
    push("anthropic-version", profile::ANTHROPIC_VERSION.into());

    let (name, value) = match ctx.auth {
        Auth::Bearer(token) => ("Authorization", format!("Bearer {token}")),
        Auth::ApiKey(key) => ("X-Api-Key", key.to_owned()),
    };
    headers.push((
        name,
        HeaderValue {
            value,
            sensitive: true,
        },
    ));
    let mut push = |name: &'static str, value: String| {
        headers.push((
            name,
            HeaderValue {
                value,
                sensitive: false,
            },
        ))
    };

    push(
        "x-app",
        if ctx.background { "cli-bg" } else { "cli" }.into(),
    );
    push(
        "User-Agent",
        user_agent(
            ctx.entrypoint,
            ctx.agent_sdk_version,
            ctx.client_app,
            ctx.workload,
        ),
    );
    push("X-Claude-Code-Session-Id", ctx.session_id.into());
    if let Some(app) = ctx.client_app.filter(|app| !app.is_empty()) {
        push("x-client-app", app.into());
    }
    if let Some(agent) = ctx.agent.filter(|agent| !agent.agent_id.is_empty()) {
        push("x-claude-code-agent-id", escape_agent_value(agent.agent_id));
        if let Some(parent) = agent.parent_agent_id.filter(|parent| !parent.is_empty()) {
            push("x-claude-code-parent-agent-id", escape_agent_value(parent));
        }
    }
    if ctx.first_party_base_url {
        push(
            "x-claude-code-request-class",
            ctx.request_class.as_str().into(),
        );
        if let Some(agent_type) = ctx.agent_type.filter(|agent_type| !agent_type.is_empty()) {
            push("x-claude-code-agent-type", escape_agent_value(agent_type));
        }
        if let Some(prompt_id) = ctx.prompt_id.filter(|id| is_uuid(id)) {
            push("x-claude-code-prompt-id", prompt_id.into());
        }
    }
    push("content-type", "application/json".into());
    if !ctx.betas.is_empty() {
        push("anthropic-beta", ctx.betas.join(","));
    }
    if ctx.first_party_base_url {
        push("x-client-request-id", ctx.client_request_id.into());
    }
    headers
}

#[cfg(test)]
#[path = "headers_tests.rs"]
mod tests;
