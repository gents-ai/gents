//! The `x-anthropic-billing-header` attribution block and its `cc_version`
//! fingerprint (Claude Code `J6o`, `chunk-1twhepc1.js`; fingerprint
//! `chunk-esz5mr3q.js`). Lean: `PromptAssembly.ClaudeAttribution`.

use sha2::{Digest, Sha256};

/// The first characters of the `system[0]` block every attribution header starts with.
pub const PREFIX: &str = "x-anthropic-billing-header:";

/// Whether a system block text is an attribution header. The header changes
/// between requests of one conversation, so it is never part of a cached or
/// replayed prefix.
pub fn is_header(text: &str) -> bool {
    text.starts_with(PREFIX)
}

/// Salt of the version fingerprint (`uH`).
const FINGERPRINT_SALT: &str = "59cf53e54c78";

/// UTF-16 code unit positions of the first user text that feed the fingerprint.
const FINGERPRINT_INDEXES: [usize; 3] = [4, 7, 20];

/// Settings layers a project attribution may come from.
pub const PROJECT_SOURCES: [&str; 7] = [
    "harness",
    "user",
    "project",
    "local",
    "flag",
    "managed",
    "admin_default",
];

/// Largest prompt or turn index Claude Code attributes (`g`).
const MAX_INDEX: u64 = 10_000_000;

/// Claude Code `Te()` provider kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    /// Anthropic's own API.
    FirstParty,
    /// Google Vertex AI.
    Vertex,
    /// AWS Bedrock.
    Bedrock,
    /// Anthropic on AWS.
    AnthropicAws,
    /// Anthropic on Google Cloud.
    AnthropicGoogleCloud,
    /// Azure AI Foundry.
    Foundry,
    /// Mantle.
    Mantle,
    /// An authenticated gateway.
    Gateway,
}

impl From<crate::betas::Provider> for Provider {
    fn from(provider: crate::betas::Provider) -> Self {
        use crate::betas::Provider as Betas;
        match provider {
            Betas::FirstParty => Self::FirstParty,
            Betas::Vertex => Self::Vertex,
            Betas::Bedrock => Self::Bedrock,
            Betas::AnthropicAws => Self::AnthropicAws,
            Betas::AnthropicGoogleCloud => Self::AnthropicGoogleCloud,
            Betas::Foundry => Self::Foundry,
            Betas::Mantle => Self::Mantle,
            Betas::Gateway => Self::Gateway,
        }
    }
}

/// The `cc_gateway` mark for a gateway bearer token (`v` in `J6o`): `1` when the
/// token's `aud` claim names `claude-gateway` (a vouched gateway), else `env`.
/// A token that is not a decodable JWT is unvouched.
pub fn gateway_mark(token: &str) -> &'static str {
    use base64::Engine as _;
    let vouched = token
        .split('.')
        .nth(1)
        .and_then(|payload| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(payload.trim_end_matches('='))
                .ok()
        })
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .is_some_and(|claims| match claims.get("aud") {
            Some(serde_json::Value::String(aud)) => aud == "claude-gateway",
            Some(serde_json::Value::Array(auds)) => auds
                .iter()
                .any(|aud| aud.as_str() == Some("claude-gateway")),
            _ => false,
        });
    if vouched {
        "1"
    } else {
        "env"
    }
}

/// A tagged project id and the settings layer it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    /// Tagged project id (`claude_proj_...`).
    pub id: String,
    /// Settings layer, one of [`PROJECT_SOURCES`].
    pub source: String,
}

/// Every input of the attribution header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fields {
    /// Client version, `cc_version` before the fingerprint.
    pub version: String,
    /// Output of [`fingerprint`].
    pub fingerprint: String,
    /// `cc_entrypoint`.
    pub entrypoint: String,
    /// Provider kind.
    pub provider: Provider,
    /// Claude Code `xi()`: the base URL is unset or `api.anthropic.com`.
    pub first_party_base_url: bool,
    /// `cron` for scheduled, looping and auto-continued work; empty is omitted.
    pub workload: Option<String>,
    /// The request comes from a subagent.
    pub subagent: bool,
    /// Previous response request id.
    pub previous_request_id: Option<String>,
    /// Prompt UUID.
    pub prompt_id: Option<String>,
    /// Turn origin tag.
    pub turn_origin: Option<String>,
    /// Prompt index; emitted only together with the turn index.
    pub prompt_index: Option<u64>,
    /// Turn index; emitted only together with the prompt index.
    pub turn_index: Option<u64>,
    /// `1` behind an authenticated gateway, `env` behind an unvouched base URL.
    pub gateway: Option<String>,
    /// Project attribution.
    pub project: Option<Project>,
}

impl Fields {
    fn first_party_base(&self) -> bool {
        self.provider == Provider::FirstParty && self.first_party_base_url
    }
}

/// `^req_[A-Za-z0-9_-]{1,36}$`.
pub fn valid_request_id(id: &str) -> bool {
    id.strip_prefix("req_").is_some_and(|rest| {
        (1..=36).contains(&rest.len())
            && rest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    })
}

/// A hyphenated UUID, case-insensitive.
pub fn valid_uuid(id: &str) -> bool {
    let mut groups = id.split('-');
    [8, 4, 4, 4, 12].iter().all(|&len| {
        groups
            .next()
            .is_some_and(|g| g.len() == len && g.bytes().all(|b| b.is_ascii_hexdigit()))
    }) && groups.next().is_none()
}

/// `^[a-z][a-z_]{0,31}$`.
pub fn valid_turn_origin(origin: &str) -> bool {
    let bytes = origin.as_bytes();
    bytes.len() <= 32
        && bytes.first().is_some_and(u8::is_ascii_lowercase)
        && bytes[1..]
            .iter()
            .all(|&b| b.is_ascii_lowercase() || b == b'_')
}

/// Claude Code `g`: an integer in `[floor, 10^7]`.
pub fn valid_index(floor: u64, value: u64) -> bool {
    (floor..=MAX_INDEX).contains(&value)
}

/// `X6o`: `^claude_proj_(?:staging_|local_)?01[1-9A-HJ-NP-Za-km-z]{22}$`.
pub fn valid_project_id(id: &str) -> bool {
    let stem = ["claude_proj_staging_", "claude_proj_local_", "claude_proj_"]
        .iter()
        .find_map(|stem| id.strip_prefix(stem));
    stem.and_then(|rest| rest.strip_prefix("01")).is_some_and(|rest| {
        rest.len() == 22
            && rest.bytes().all(|b| {
                matches!(b, b'1'..=b'9' | b'A'..=b'H' | b'J'..=b'N' | b'P'..=b'Z' | b'a'..=b'k' | b'm'..=b'z')
            })
    })
}

/// The `cc_version` fingerprint: SHA-256 over the salt, UTF-16 units 4, 7 and 20
/// of `first_user_text` (`"0"` for a missing unit) and `version`, first 3 hex digits.
///
/// The caller passes the text of the first non-meta user message: its string
/// content, or the first text block of its content array, else the empty string.
pub fn fingerprint(first_user_text: &str, version: &str) -> String {
    let units: Vec<u16> = first_user_text.encode_utf16().take(21).collect();
    let picked: Vec<u16> = FINGERPRINT_INDEXES
        .iter()
        .map(|&i| units.get(i).copied().unwrap_or(u16::from(b'0')))
        .collect();
    // Node hashes the joined JS string as UTF-8 with unpaired surrogates as U+FFFD.
    let joined = String::from_utf16_lossy(&picked);
    let mut hasher = Sha256::new();
    hasher.update(FINGERPRINT_SALT.as_bytes());
    hasher.update(joined.as_bytes());
    hasher.update(version.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(3);
    for byte in &digest[..2] {
        out.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        out.push(char::from_digit(u32::from(byte & 15), 16).unwrap_or('0'));
    }
    out.truncate(3);
    out
}

/// The `system[0]` attribution text, byte for byte the Lean `header`.
pub fn header(fields: &Fields) -> String {
    let base = fields.first_party_base();
    let mut out = String::with_capacity(256);
    out.push_str(PREFIX);
    out.push_str(" cc_version=");
    out.push_str(&fields.version);
    out.push('.');
    out.push_str(&fields.fingerprint);
    out.push_str("; cc_entrypoint=");
    out.push_str(&fields.entrypoint);
    out.push(';');
    if base || fields.provider == Provider::Vertex {
        out.push_str(" cch=00000;");
    }
    if let Some(workload) = fields.workload.as_deref().filter(|w| !w.is_empty()) {
        out.push_str(" cc_workload=");
        out.push_str(workload);
        out.push(';');
    }
    if fields.subagent {
        out.push_str(" cc_is_subagent=true;");
    }
    if base {
        if let Some(id) = fields
            .previous_request_id
            .as_deref()
            .filter(|id| valid_request_id(id))
        {
            out.push_str(" cc_prev_req=");
            out.push_str(id);
            out.push(';');
        }
        if let Some(id) = fields.prompt_id.as_deref().filter(|id| valid_uuid(id)) {
            out.push_str(" cc_prompt_id=");
            out.push_str(id);
            out.push(';');
        }
        if let Some(origin) = fields
            .turn_origin
            .as_deref()
            .filter(|o| valid_turn_origin(o))
        {
            out.push_str(" cc_turn_origin=");
            out.push_str(origin);
            out.push(';');
        }
        if let (Some(prompt), Some(turn)) = (fields.prompt_index, fields.turn_index) {
            if valid_index(0, prompt) && valid_index(1, turn) {
                out.push_str(" cc_prompt_index=");
                out.push_str(&prompt.to_string());
                out.push_str("; cc_turn_index=");
                out.push_str(&turn.to_string());
                out.push(';');
            }
        }
    }
    if let Some(mark) = &fields.gateway {
        out.push_str(" cc_gateway=");
        out.push_str(mark);
        out.push(';');
    }
    let project = fields.project.as_ref().filter(|project| {
        base && valid_project_id(&project.id) && PROJECT_SOURCES.contains(&project.source.as_str())
    });
    if let Some(project) = project {
        out.push_str(" cc_project=");
        out.push_str(&project.id);
        out.push_str("; cc_project_src=");
        out.push_str(&project.source);
        out.push(';');
    }
    out
}

#[cfg(test)]
#[path = "attribution_tests.rs"]
mod tests;
