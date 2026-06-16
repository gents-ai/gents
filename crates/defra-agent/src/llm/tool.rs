//! Native tool trait + definition, mirroring rig's `tool::{Tool, ToolDyn,
//! ToolError}` and `completion::ToolDefinition`. defra-agent is not a wasm
//! target, so the wasm-compat bounds reduce to `Send`/`Sync` and the boxed
//! future is a plain [`BoxFuture`].
//!
//! Tools implement [`Tool`] (typed args/output); the blanket impl gives every
//! `Tool` a dyn-safe [`ToolDyn`] (string-in / string-out) that the owned loop
//! dispatches. See `docs/design/native-llm-types-shed-rig.md` (removed from the tree; see git history).

use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};

/// Boxed, `Send` future — the off-wasm form of rig's `BoxFuture`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A tool's name, description, and JSON-schema parameters, sent to the provider.
/// Mirrors rig's `completion::ToolDefinition`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Error from dyn tool dispatch: the tool itself failed, or args/output failed
/// to de/serialize. Mirrors rig's `tool::ToolError`.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// Error returned by the tool's own `call`.
    #[error("tool call error: {0}")]
    ToolCallError(#[from] Box<dyn std::error::Error + Send + Sync>),
    /// Arguments or output failed to de/serialize.
    #[error("tool json error: {0}")]
    JsonError(#[from] serde_json::Error),
    /// The model's tool-call `arguments` string could not be parsed even after a
    /// tolerant repair pass. This is the CLIENT-side analogue of vLLM's
    /// server-side tool-call JSON-parse 400 (see
    /// [`crate::error::classify_completion_error`]): the payload was malformed
    /// (a lone-backslash escape the model emitted raw) or truncated by the
    /// generation hitting its token cap (`finish_reason == "length"`, surfaced
    /// here as a [`serde_json`] `Category::Eof`). It is intermittent and
    /// sampling-dependent, so callers should treat it as a transient/retryable
    /// signal and re-run the inference rather than feeding the parse error back
    /// to the model as the tool result (which only makes the model re-emit the
    /// same broken payload until the budget is spent and nothing is posted).
    #[error("tool args unparseable ({kind}): {reason}")]
    UnparseableArgs {
        kind: UnparseableArgsKind,
        reason: String,
    },
}

impl ToolError {
    /// Whether this failure is worth retrying with a fresh generation. Only the
    /// transient, sampling-dependent argument-parse failures are retryable; a
    /// tool's own error or an output-serialization bug is not.
    pub fn is_retryable(&self) -> bool {
        matches!(self, ToolError::UnparseableArgs { .. })
    }
}

/// Why a tool-call `arguments` string was unparseable, mapped from the failing
/// [`serde_json`] error category. Distinguishes a truncated payload (the
/// `finish_reason == "length"` shape) from a structurally malformed one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnparseableArgsKind {
    /// The arguments ended mid-value: the generation hit its token cap
    /// (`finish_reason == "length"`) before closing the JSON. Surfaced by
    /// `serde_json` as `Category::Eof`.
    Truncated,
    /// The arguments were structurally malformed (e.g. a lone-backslash escape
    /// the model emitted raw) and the tolerant repair pass could not recover a
    /// value that deserializes into the tool's `Args`.
    Malformed,
}

impl std::fmt::Display for UnparseableArgsKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UnparseableArgsKind::Truncated => f.write_str("truncated"),
            UnparseableArgsKind::Malformed => f.write_str("malformed"),
        }
    }
}

/// A typed tool: deserializes `Args`, runs, serializes `Output`. Mirrors rig's
/// `tool::Tool`.
pub trait Tool: Sized + Send + Sync {
    /// Unique tool name.
    const NAME: &'static str;
    /// The tool's error type.
    type Error: std::error::Error + Send + Sync + 'static;
    /// The tool's argument type (deserialized from the model's JSON).
    type Args: for<'a> Deserialize<'a> + Send + Sync;
    /// The tool's output type (serialized back to the model).
    type Output: Serialize;

    /// The tool's name (defaults to [`Tool::NAME`]).
    fn name(&self) -> String {
        Self::NAME.to_string()
    }

    /// The tool's definition; `prompt` may tailor it.
    fn definition(&self, prompt: String) -> impl Future<Output = ToolDefinition> + Send + Sync;

    /// Execute the tool.
    fn call(
        &self,
        args: Self::Args,
    ) -> impl Future<Output = Result<Self::Output, Self::Error>> + Send;
}

/// Dyn-safe, string-in/string-out tool the loop dispatches. Mirrors rig's
/// `tool::ToolDyn`.
pub trait ToolDyn: Send + Sync {
    fn name(&self) -> String;
    fn definition<'a>(&'a self, prompt: String) -> BoxFuture<'a, ToolDefinition>;
    fn call<'a>(&'a self, args: String) -> BoxFuture<'a, Result<String, ToolError>>;
}

fn serialize_tool_output(output: impl Serialize) -> serde_json::Result<String> {
    match serde_json::to_value(output)? {
        serde_json::Value::String(text) => Ok(text),
        value => Ok(value.to_string()),
    }
}

impl<T: Tool> ToolDyn for T {
    fn name(&self) -> String {
        Tool::name(self)
    }

    fn definition<'a>(&'a self, prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(<Self as Tool>::definition(self, prompt))
    }

    fn call<'a>(&'a self, args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            let parsed = parse_tool_args::<<Self as Tool>::Args>(&args)?;
            <Self as Tool>::call(self, parsed)
                .await
                .map_err(|error| ToolError::ToolCallError(Box::new(error)))
                .and_then(|output| serialize_tool_output(output).map_err(ToolError::JsonError))
        })
    }
}

/// Deserialize a tool's `Args` from the model's raw `arguments` string, applying
/// one tolerant [`repair_tool_arguments`] pass when the raw string fails to
/// parse.
///
/// The model occasionally emits an `arguments` string that Rust's `serde_json`
/// rejects: a lone backslash that is not a legal JSON escape (the "raw-escape"
/// shape stewards hit on markdown bodies with `\d+` regexes / `C:\path`s), or a
/// payload truncated mid-value because the generation hit its token cap
/// (`finish_reason == "length"`). Rather than feed that error straight back to
/// the model as the tool result — which only makes it re-emit the same broken
/// payload — we try a conservative repair and re-parse. If the repair still does
/// not yield a value that deserializes into `Args`, we raise the typed,
/// retryable [`ToolError::UnparseableArgs`] so the run re-attempts the inference.
fn parse_tool_args<A>(args: &str) -> Result<A, ToolError>
where
    A: for<'de> Deserialize<'de>,
{
    // The happy path: a clean `arguments` string parses with no repair.
    let first_error = match serde_json::from_str::<A>(args) {
        Ok(value) => return Ok(value),
        Err(error) => error,
    };

    // Attempt one tolerant repair pass, then re-parse into `Args`. We only accept
    // the repaired value if it actually deserializes into the tool's typed args;
    // a repair that produces structurally-valid-but-incomplete JSON (e.g. a
    // truncated object missing a required field) must NOT be silently accepted.
    if let Some(repaired) = repair_tool_arguments(args) {
        if let Ok(value) = serde_json::from_str::<A>(&repaired) {
            return Ok(value);
        }
    }

    // Irrecoverable. Classify the original failure so callers can tell a
    // truncated payload (the `finish_reason == "length"` shape) from a malformed
    // one, and surface a typed retryable signal instead of the bare parse error.
    Err(unparseable_args_error(&first_error))
}

/// Map a failing [`serde_json::Error`] to the typed, retryable
/// [`ToolError::UnparseableArgs`]. A `Category::Eof` failure is the parse-seam
/// fingerprint of a `finish_reason == "length"` truncation (the generation hit
/// its token cap mid-arguments); everything else is treated as malformed.
fn unparseable_args_error(error: &serde_json::Error) -> ToolError {
    let kind = match error.classify() {
        serde_json::error::Category::Eof => UnparseableArgsKind::Truncated,
        _ => UnparseableArgsKind::Malformed,
    };
    ToolError::UnparseableArgs {
        kind,
        reason: error.to_string(),
    }
}

/// Conservatively repair a tool-call `arguments` string the model emitted in a
/// shape Rust's `serde_json` rejects, returning the repaired string only if a
/// repair was applied. Two narrow, well-understood corruptions are handled:
///
/// 1. **Lone backslashes.** The model writes a single backslash that is not a
///    legal JSON escape (`\d`, `C:\temp`, a bare `\` before a normal char). We
///    walk the string and double any backslash that does not introduce a valid
///    JSON escape (`\" \\ \/ \b \f \n \r \t \uXXXX`), turning the raw output
///    into the escaped form the model should have emitted.
/// 2. **Trailing truncation.** The generation stopped mid-value
///    (`finish_reason == "length"`), leaving an unterminated string and unclosed
///    objects/arrays. We close a dangling string then close every still-open
///    `{`/`[` in reverse order so the result is at least structurally valid
///    JSON. (Whether it then deserializes into the tool's `Args` — e.g. whether
///    a required field survived the cut — is decided by the re-parse in
///    [`parse_tool_args`], which rejects an incomplete object.)
///
/// The pass is deliberately limited to these two cases; it does not attempt to
/// repair arbitrary invalid JSON. Returns `None` when the input is already
/// well-formed enough that no repair was needed (the caller has already tried a
/// clean parse, so there is nothing to gain from re-parsing an identical string).
pub fn repair_tool_arguments(raw: &str) -> Option<String> {
    let escaped = escape_lone_backslashes(raw);
    let closed = close_truncated_json(&escaped);
    if closed == raw {
        None
    } else {
        Some(closed)
    }
}

/// Double any backslash that does not begin a valid JSON escape sequence, so the
/// model's raw single-backslash output (`\d`, `C:\temp`) becomes legal JSON. A
/// backslash that already introduces a valid escape (`\"`, `\\`, `\n`, `ꯍ`,
/// …) is left untouched, and the second backslash of a legal `\\` pair is
/// consumed so it is not re-escaped.
fn escape_lone_backslashes(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = String::with_capacity(raw.len() + 8);
    let mut chars = raw.char_indices().peekable();
    while let Some((idx, ch)) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match bytes.get(idx + 1).copied() {
            // A legal single-char JSON escape: emit the pair verbatim and consume
            // the escaped char so it is not reconsidered.
            Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => {
                out.push('\\');
                if let Some((_, next)) = chars.next() {
                    out.push(next);
                }
            }
            // A `\u` escape is legal only when followed by four hex digits.
            Some(b'u') if is_valid_unicode_escape(bytes, idx) => {
                out.push('\\');
                if let Some((_, next)) = chars.next() {
                    out.push(next);
                }
            }
            // A lone backslash (invalid escape, or a trailing `\` at EOF): double
            // it so the literal backslash is preserved as legal JSON.
            _ => out.push_str("\\\\"),
        }
    }
    out
}

/// Whether the bytes at `backslash_idx` form a valid `\uXXXX` escape (a `u`
/// followed by exactly four ASCII hex digits).
fn is_valid_unicode_escape(bytes: &[u8], backslash_idx: usize) -> bool {
    let hex_start = backslash_idx + 2;
    bytes
        .get(hex_start..hex_start + 4)
        .is_some_and(|hex| hex.iter().all(u8::is_ascii_hexdigit))
}

/// Close a JSON value that was truncated mid-generation: terminate a dangling
/// string, then close every still-open `{` / `[` in reverse nesting order. Input
/// that is not inside an unterminated string and has balanced brackets is
/// returned unchanged. Operates on a string whose backslashes are already
/// JSON-legal (run [`escape_lone_backslashes`] first).
fn close_truncated_json(input: &str) -> String {
    let mut stack: Vec<char> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    for ch in input.chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' => {
                stack.pop();
            }
            _ => {}
        }
    }

    if !in_string && stack.is_empty() {
        return input.to_string();
    }

    let mut out = input.to_string();
    // A dangling escape (`...\`) at EOF would make the closing quote part of the
    // escape; drop it so the quote terminates the string cleanly.
    if escaped {
        out.pop();
    }
    if in_string {
        out.push('"');
    }
    while let Some(closer) = stack.pop() {
        out.push(closer);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    struct Sample {
        report_type: String,
        body: String,
        findings: Vec<String>,
    }

    fn valid_sample_json() -> String {
        serde_json::json!({
            "report_type": "steward",
            "body": "ok",
            "findings": ["a", "b"]
        })
        .to_string()
    }

    #[test]
    fn repair_passes_through_valid_json_unchanged() {
        // A well-formed string needs no repair; the helper returns None so the
        // caller does not re-parse an identical string.
        assert!(repair_tool_arguments(&valid_sample_json()).is_none());
    }

    #[test]
    fn repair_escapes_lone_backslash_into_valid_json() {
        // A lone backslash before `d` (the model's raw `\d+` regex) is not a legal
        // JSON escape; repair doubles it so the literal backslash survives.
        let raw = r#"{"body":"regex \d+ here"}"#;
        let repaired = repair_tool_arguments(raw).expect("lone backslash should be repaired");
        let value: serde_json::Value =
            serde_json::from_str(&repaired).expect("repaired payload must parse");
        assert_eq!(value["body"], "regex \\d+ here");
    }

    #[test]
    fn repair_preserves_valid_escapes_and_only_fixes_lone_backslashes() {
        // A mix: legal `\n` and `\"` stay; the raw `\d` and trailing `\` get
        // doubled. The result must parse and keep the legal escapes' meaning.
        let raw = r#"{"body":"line\nwith \d and a quote \" and tail \"}"#;
        let repaired = repair_tool_arguments(raw).expect("should repair the lone backslashes");
        let value: serde_json::Value =
            serde_json::from_str(&repaired).expect("repaired payload must parse");
        let body = value["body"].as_str().unwrap();
        assert!(
            body.contains('\n'),
            "legal \\n escape must survive as a newline"
        );
        assert!(
            body.contains("\\d"),
            "raw \\d must become a literal backslash-d"
        );
        assert!(
            body.contains('"'),
            "legal \\\" escape must survive as a quote"
        );
    }

    #[test]
    fn repair_keeps_unicode_escape_intact() {
        // `é` is a legal escape and must not be doubled.
        let raw = r#"{"body":"café \d"}"#;
        let repaired = repair_tool_arguments(raw).expect("the lone \\d should trigger a repair");
        let value: serde_json::Value =
            serde_json::from_str(&repaired).expect("repaired payload must parse");
        assert_eq!(value["body"], "café \\d");
    }

    #[test]
    fn repair_closes_object_truncated_mid_string() {
        // Generation stopped inside a string value: close the string and the
        // object so the result is structurally valid JSON.
        let raw = r#"{"report_type":"steward","body":"partial body that got cut"#;
        let repaired = repair_tool_arguments(raw).expect("truncation should be repaired");
        assert!(
            serde_json::from_str::<serde_json::Value>(&repaired).is_ok(),
            "repaired truncated object must be valid JSON: {repaired}"
        );
    }

    #[test]
    fn repair_closes_nested_array_and_object() {
        // Truncated inside a nested array: every still-open bracket is closed in
        // reverse order.
        let raw = r#"{"findings":["one","two"#;
        let repaired = repair_tool_arguments(raw).expect("nested truncation should be repaired");
        let value: serde_json::Value =
            serde_json::from_str(&repaired).expect("repaired payload must parse");
        assert!(value["findings"].is_array());
    }

    #[test]
    fn parse_tool_args_recovers_repairable_payload() {
        // A payload with an unambiguously-invalid lone backslash (`\d`, the
        // steward regex case) AND all required fields present should be repaired
        // and deserialize into the typed args, preserving the literal backslash.
        let raw = r#"{"report_type":"steward","body":"regex \d+ here","findings":["x"]}"#;
        let parsed: Sample = parse_tool_args(raw).expect("repairable payload should parse");
        assert_eq!(parsed.report_type, "steward");
        assert_eq!(parsed.body, "regex \\d+ here");
    }

    #[test]
    fn parse_tool_args_truncated_is_retryable_truncated_kind() {
        // Truncated mid-string with the required `findings`/`body` cut off:
        // repair makes it structurally valid but it cannot deserialize into the
        // typed args, so we surface the typed retryable signal, kinded Truncated
        // (the finish_reason=length / serde Category::Eof shape).
        let raw = r#"{"report_type":"steward","body":"a long body that got cut o"#;
        let error = parse_tool_args::<Sample>(raw).expect_err("truncated payload must not parse");
        match error {
            ToolError::UnparseableArgs { kind, .. } => {
                assert_eq!(kind, UnparseableArgsKind::Truncated);
                assert!(ToolError::UnparseableArgs {
                    kind,
                    reason: String::new()
                }
                .is_retryable());
            }
            other => panic!("expected UnparseableArgs, got {other:?}"),
        }
    }

    #[test]
    fn parse_tool_args_malformed_is_retryable_malformed_kind() {
        // A complete-but-malformed object (a lone backslash the repair cannot make
        // deserialize into the typed args because a required field is the wrong
        // type) is classified Malformed, still retryable. Here `findings` is a
        // string, not an array — a non-Eof, non-repairable shape.
        let raw = r#"{"report_type":"steward","body":"ok","findings":"not-an-array"}"#;
        let error =
            parse_tool_args::<Sample>(raw).expect_err("type-mismatched payload must not parse");
        match error {
            ToolError::UnparseableArgs { kind, .. } => {
                assert_eq!(kind, UnparseableArgsKind::Malformed);
                assert!(error.is_retryable());
            }
            other => panic!("expected UnparseableArgs, got {other:?}"),
        }
    }

    #[test]
    fn unparseable_args_error_maps_eof_to_truncated() {
        let eof = serde_json::from_str::<serde_json::Value>("{\"a\":").unwrap_err();
        assert!(matches!(
            unparseable_args_error(&eof),
            ToolError::UnparseableArgs {
                kind: UnparseableArgsKind::Truncated,
                ..
            }
        ));
    }
}
