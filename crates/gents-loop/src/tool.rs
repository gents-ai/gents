use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};

#[cfg(test)]
use crate::test_support;
use crate::tool_call_lifecycle::FailureClass;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("tool call error: {0}")]
    ToolCallError(#[from] Box<dyn std::error::Error + Send + Sync>),
    #[error("tool json error: {0}")]
    JsonError(#[from] serde_json::Error),
    /// The model's tool-call `arguments` string could not be parsed even after a
    /// tolerant (escape-only) repair pass: the payload was malformed (a
    /// lone-backslash escape the model emitted raw that the repair could not make
    /// deserialize). Provider dispatch may classify syntax failure as truncated
    /// only when that attempt reports a token limit. Schema failures remain
    /// distinct from JSON syntax errors.
    #[error("tool args unparseable ({kind}): {reason}")]
    UnparseableArgs {
        kind: UnparseableArgsKind,
        reason: String,
    },
    /// A trusted tool adapter observed an execution failure and rendered the
    /// model-facing detail itself.
    #[error("{text}")]
    ReportedFailure { class: FailureClass, text: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnparseableArgsKind {
    Truncated,
    Malformed,
    UnknownField,
    MissingField,
    WrongType,
    Schema,
}

impl std::fmt::Display for UnparseableArgsKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UnparseableArgsKind::Truncated => f.write_str("truncated"),
            UnparseableArgsKind::Malformed => f.write_str("malformed"),
            UnparseableArgsKind::UnknownField => f.write_str("unknown field"),
            UnparseableArgsKind::MissingField => f.write_str("missing field"),
            UnparseableArgsKind::WrongType => f.write_str("wrong type"),
            UnparseableArgsKind::Schema => f.write_str("schema mismatch"),
        }
    }
}

pub trait Tool: Sized + Send + Sync {
    const NAME: &'static str;
    /// See [`ToolDyn::emits_command_envelope`].
    const EMITS_COMMAND_ENVELOPE: bool = false;
    type Error: std::error::Error + Send + Sync + 'static;
    type Args: for<'a> Deserialize<'a> + Send + Sync;
    type Output: Serialize;

    fn name(&self) -> String {
        Self::NAME.to_string()
    }

    fn definition(&self, prompt: String) -> impl Future<Output = ToolDefinition> + Send + Sync;

    fn call(
        &self,
        args: Self::Args,
    ) -> impl Future<Output = Result<Self::Output, Self::Error>> + Send;

    /// Policy admission for these arguments: side-effect free, evaluated
    /// before the dispatch election (a rejection leaves the call Pending as a
    /// pre-dispatch failure), and must reject only what `call` would reject
    /// before any external effect.
    fn admit(&self, _args: &Self::Args) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Convert the concrete tool error at the trusted adapter boundary.
    fn into_dyn_error(error: Self::Error) -> ToolError {
        ToolError::ToolCallError(Box::new(error))
    }
}

/// Host-owned execution evidence is carried separately from model-facing text.
#[derive(Debug)]
pub struct ToolDispatchResult {
    pub result: Result<String, ToolError>,
    pub plugin_receipt: Option<gents_protocol::plugin::PluginExecutionReceipt>,
}

pub trait ToolDyn: Send + Sync {
    fn name(&self) -> String;
    fn definition<'a>(&'a self, prompt: String) -> BoxFuture<'a, ToolDefinition>;
    fn call<'a>(&'a self, args: String) -> BoxFuture<'a, Result<String, ToolError>>;

    fn call_with_receipt<'a>(&'a self, args: String) -> BoxFuture<'a, ToolDispatchResult> {
        Box::pin(async move {
            ToolDispatchResult {
                result: self.call(args).await,
                plugin_receipt: None,
            }
        })
    }

    /// True only for the built-in command runner (its results render command
    /// output metadata; any other tool's text is its own).
    fn emits_command_envelope(&self) -> bool {
        false
    }

    /// See [`Tool::admit`].
    fn admit(&self, _args: &str) -> Result<(), ToolError> {
        Ok(())
    }
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

    fn emits_command_envelope(&self) -> bool {
        T::EMITS_COMMAND_ENVELOPE
    }

    fn definition<'a>(&'a self, prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(<Self as Tool>::definition(self, prompt))
    }

    fn call<'a>(&'a self, args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            let parsed = parse_tool_args::<<Self as Tool>::Args>(&args)?;
            <Self as Tool>::call(self, parsed)
                .await
                .map_err(<Self as Tool>::into_dyn_error)
                .and_then(|output| serialize_tool_output(output).map_err(ToolError::JsonError))
        })
    }

    fn admit(&self, args: &str) -> Result<(), ToolError> {
        match parse_tool_args::<<Self as Tool>::Args>(args) {
            Ok(parsed) => {
                <Self as Tool>::admit(self, &parsed).map_err(<Self as Tool>::into_dyn_error)
            }
            Err(_) => Ok(()),
        }
    }
}

/// Deserialize a tool's `Args` from the model's raw `arguments` string, applying
/// one tolerant [`repair_tool_arguments`] pass when the raw string fails to
/// parse.
///
/// Repair changes escaping only; it never completes a cut-off value. Syntax
/// failures remain malformed unless provider dispatch has a token-limit signal
/// from the same attempt. Complete JSON receives typed schema validation.
// pub, not pub(crate): gents' native tool call sites (eth, meta_tools) parse
// their typed arguments through this, and a pub(crate) item in this crate is
// invisible outside it.
pub fn parse_tool_args<A>(args: &str) -> Result<A, ToolError>
where
    A: for<'de> Deserialize<'de>,
{
    let value = match serde_json::from_str::<serde_json::Value>(args) {
        Ok(value) => value,
        Err(first_error) => {
            // Repair only JSON syntax; once valid, typed deserialization
            // reports its schema problem directly.
            if let Some(repaired) = repair_tool_arguments(args) {
                serde_json::from_str::<serde_json::Value>(&repaired)
                    .map_err(|second_error| unparseable_args_error(&second_error))?
            } else {
                return Err(unparseable_args_error(&first_error));
            }
        }
    };

    serde_json::from_value::<A>(value).map_err(schema_args_error)
}

/// Map a failing [`serde_json::Error`] to the typed [`ToolError::UnparseableArgs`].
fn unparseable_args_error(error: &serde_json::Error) -> ToolError {
    ToolError::UnparseableArgs {
        kind: UnparseableArgsKind::Malformed,
        reason: error.to_string(),
    }
}

pub(crate) fn with_provider_token_limit(error: ToolError, token_limit: bool) -> ToolError {
    match error {
        ToolError::UnparseableArgs {
            kind: UnparseableArgsKind::Malformed,
            reason,
        } if token_limit => ToolError::UnparseableArgs {
            kind: UnparseableArgsKind::Truncated,
            reason,
        },
        other => other,
    }
}

fn schema_args_error(error: serde_json::Error) -> ToolError {
    let raw = error.to_string();
    let kind = if raw.starts_with("unknown field") {
        UnparseableArgsKind::UnknownField
    } else if raw.starts_with("missing field") {
        UnparseableArgsKind::MissingField
    } else if raw.starts_with("invalid type") {
        UnparseableArgsKind::WrongType
    } else {
        UnparseableArgsKind::Schema
    };
    // Unknown/missing-field diagnostics contain only schema vocabulary.
    // Type/value diagnostics can echo the supplied value, so retain only the
    // declared expectation — never reflect arbitrary argument contents
    // (which may include credentials) in retry guidance.
    let reason = match kind {
        UnparseableArgsKind::UnknownField | UnparseableArgsKind::MissingField => raw,
        UnparseableArgsKind::WrongType => raw
            .split_once(", expected ")
            .map(|(_, expected)| format!("wrong JSON type; expected {expected}"))
            .unwrap_or_else(|| "wrong JSON type for a declared field".to_string()),
        UnparseableArgsKind::Schema => {
            "arguments do not match the declared tool schema".to_string()
        }
        UnparseableArgsKind::Truncated | UnparseableArgsKind::Malformed => unreachable!(),
    };
    let reason = reason.chars().take(512).collect();
    ToolError::UnparseableArgs { kind, reason }
}

/// Conservatively repair a tool-call `arguments` string the model emitted in
/// a shape `serde_json` rejects, returning the repaired string only if a
/// repair was applied. Two corruptions are handled, both escapes:
///
/// 1. **Lone backslashes**: double any backslash that does not introduce a
///    valid JSON escape (`\" \\ \/ \b \f \n \r \t \uXXXX`).
/// 2. **Literal control characters inside string literals** (#589): escape
///    raw `0x00-0x1f` bytes (`\n`, `\r`, …, `\uXXXX`); identity on valid
///    payloads. Control characters BETWEEN tokens are legal whitespace and
///    are left untouched. Runs after backslash-doubling so in-string
///    tracking is unambiguous.
///
/// Escape-only by design: it never closes a truncated value (dangling
/// string / unbalanced brackets). Closing a truncation could let a
/// half-written value run as if whole; by refusing to close, a
/// `finish_reason == "length"` payload stays unparseable and surfaces as
/// [`ToolError::UnparseableArgs`].
///
/// Returns `None` when neither corruption was found (nothing to re-parse).
pub fn repair_tool_arguments(raw: &str) -> Option<String> {
    let escaped = escape_lone_backslashes(raw);
    let escaped = escape_control_characters_in_strings(&escaped);
    if escaped == raw {
        None
    } else {
        Some(escaped)
    }
}

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
            Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => {
                out.push('\\');
                if let Some((_, next)) = chars.next() {
                    out.push(next);
                }
            }
            Some(b'u') if is_valid_unicode_escape(bytes, idx) => {
                out.push('\\');
                if let Some((_, next)) = chars.next() {
                    out.push(next);
                }
            }
            _ => out.push_str("\\\\"),
        }
    }
    out
}

fn is_valid_unicode_escape(bytes: &[u8], backslash_idx: usize) -> bool {
    let hex_start = backslash_idx + 2;
    bytes
        .get(hex_start..hex_start + 4)
        .is_some_and(|hex| hex.iter().all(u8::is_ascii_hexdigit))
}

fn escape_control_characters_in_strings(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 8);
    let mut in_string = false;
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        if !in_string {
            if ch == '"' {
                in_string = true;
            }
            out.push(ch);
            continue;
        }
        match ch {
            '\\' => {
                out.push(ch);
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            }
            '"' => {
                in_string = false;
                out.push(ch);
            }
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000c}' => out.push_str("\\f"),
            control if (control as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", control as u32));
            }
            other => out.push(other),
        }
    }
    out
}

/// Normalize a tool-call `arguments` value to the provider-valid **object**
/// shape (Lean `PromptAssembly.normalizeArgs`, issues #589/#590): an object
/// passes through unchanged (N2); `null` and empty/whitespace strings
/// become `{}`; a string that parses after the tolerant
/// [`repair_tool_arguments`] pass to an object becomes that object (N4);
/// anything else becomes `{}` with a bounded warning (N1). Applied at both
/// rig-converter seams (ingest — nothing non-object is accumulated into
/// durable history — and egress — already-poisoned durable history
/// self-heals at request build). `seam` labels the boundary in the warning.
pub fn normalize_tool_call_arguments(
    seam: &'static str,
    tool_name: &str,
    arguments: &serde_json::Value,
) -> serde_json::Value {
    use serde_json::Value;
    match arguments {
        Value::Object(_) => arguments.clone(),
        Value::Null => Value::Object(serde_json::Map::new()),
        Value::String(raw) if raw.trim().is_empty() => Value::Object(serde_json::Map::new()),
        Value::String(raw) => {
            let parsed = serde_json::from_str::<Value>(raw).ok().or_else(|| {
                repair_tool_arguments(raw)
                    .and_then(|repaired| serde_json::from_str::<Value>(&repaired).ok())
            });
            match parsed {
                Some(Value::Object(map)) => Value::Object(map),
                _ => {
                    warn_nonobject_arguments_coerced(seam, tool_name, arguments);
                    Value::Object(serde_json::Map::new())
                }
            }
        }
        _ => {
            warn_nonobject_arguments_coerced(seam, tool_name, arguments);
            Value::Object(serde_json::Map::new())
        }
    }
}

const COERCION_WARN_SNIPPET_CHARS: usize = 256;

fn warn_nonobject_arguments_coerced(seam: &str, tool_name: &str, arguments: &serde_json::Value) {
    let rendered = arguments.to_string();
    let truncated: String = rendered.chars().take(COERCION_WARN_SNIPPET_CHARS).collect();
    tracing::warn!(
        seam,
        tool = tool_name,
        payload_bytes = rendered.len(),
        payload = %truncated,
        "non-object tool-call arguments coerced to an empty object at the provider boundary"
    );
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
        assert!(repair_tool_arguments(&valid_sample_json()).is_none());
    }

    #[test]
    fn repair_escapes_lone_backslash_into_valid_json() {
        let raw = r#"{"body":"regex \d+ here"}"#;
        let repaired = repair_tool_arguments(raw).expect("lone backslash should be repaired");
        let value: serde_json::Value =
            serde_json::from_str(&repaired).expect("repaired payload must parse");
        assert_eq!(value["body"], "regex \\d+ here");
    }

    #[test]
    fn repair_preserves_valid_escapes_and_only_fixes_lone_backslashes() {
        let raw = r#"{"body":"line\nwith \d and a quote \" here"}"#;
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
        let raw = r#"{"body":"café \d"}"#;
        let repaired = repair_tool_arguments(raw).expect("the lone \\d should trigger a repair");
        let value: serde_json::Value =
            serde_json::from_str(&repaired).expect("repaired payload must parse");
        assert_eq!(value["body"], "café \\d");
    }

    #[test]
    fn repair_does_not_close_truncation() {
        let raw = r#"{"report_type":"steward","body":"partial body that got cut"#;
        assert!(
            repair_tool_arguments(raw).is_none(),
            "truncation must not be repaired into valid JSON"
        );
    }

    #[test]
    fn parse_tool_args_recovers_repairable_payload() {
        let raw = r#"{"report_type":"steward","body":"regex \d+ here","findings":["x"]}"#;
        let parsed: Sample = parse_tool_args(raw).expect("repairable payload should parse");
        assert_eq!(parsed.report_type, "steward");
        assert_eq!(parsed.body, "regex \\d+ here");
    }

    #[test]
    fn parse_tool_args_incomplete_without_provider_reason_is_malformed() {
        let raw = r#"{"report_type":"steward","body":"a long body that got cut o"#;
        let error = parse_tool_args::<Sample>(raw).expect_err("truncated payload must not parse");
        assert!(matches!(
            error,
            ToolError::UnparseableArgs {
                kind: UnparseableArgsKind::Malformed,
                ..
            }
        ));
    }

    #[test]
    fn parse_tool_args_wrong_type_reports_schema_kind() {
        let raw = r#"{"report_type":"steward","body":"ok","findings":"not-an-array"}"#;
        let error =
            parse_tool_args::<Sample>(raw).expect_err("type-mismatched payload must not parse");
        assert!(matches!(
            error,
            ToolError::UnparseableArgs {
                kind: UnparseableArgsKind::WrongType,
                ..
            }
        ));
    }

    #[test]
    fn parse_tool_args_distinguishes_unknown_and_missing_fields() {
        #[derive(Debug, Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Args {
            #[allow(dead_code)]
            argv: Vec<String>,
        }

        for (raw, expected) in [
            (
                r#"{"persona_name":"review"}"#,
                UnparseableArgsKind::UnknownField,
            ),
            (r#"{}"#, UnparseableArgsKind::MissingField),
        ] {
            let error = parse_tool_args::<Args>(raw).expect_err("schema must reject");
            assert!(matches!(
                error,
                ToolError::UnparseableArgs { kind, .. } if kind == expected
            ));
        }

        let error = parse_tool_args::<Args>(r#"{"argv":"credential-like-value"}"#)
            .expect_err("wrong type must reject");
        let ToolError::UnparseableArgs { kind, reason } = error else {
            panic!("typed schema error required")
        };
        assert_eq!(kind, UnparseableArgsKind::WrongType);
        assert!(reason.contains("expected a sequence"), "{reason}");
        assert!(!reason.contains("credential-like-value"), "{reason}");
    }

    #[test]
    fn unparseable_args_error_does_not_infer_token_limit_from_eof() {
        let eof = serde_json::from_str::<serde_json::Value>("{\"a\":").unwrap_err();
        assert!(matches!(
            unparseable_args_error(&eof),
            ToolError::UnparseableArgs {
                kind: UnparseableArgsKind::Malformed,
                ..
            }
        ));
    }

    #[derive(Debug, Deserialize)]
    struct SingleField {
        #[allow(dead_code)]
        note: String,
    }

    #[test]
    fn parse_tool_args_truncation_in_last_field_is_not_run() {
        // A payload truncated INSIDE its last present field: if the repair closed
        // the dangling string it would yield `{"note":"…"}`, which DOES deserialize
        // into the tool's `Args` — and running it would silently commit a
        // half-written value. Escape-only repair refuses to close, so this never
        // runs.
        let raw = r#"{"note":"a long note that got cut o"#;
        let error = parse_tool_args::<SingleField>(raw)
            .expect_err("a truncated-but-would-type-check payload must NOT run");
        assert!(matches!(
            error,
            ToolError::UnparseableArgs {
                kind: UnparseableArgsKind::Malformed,
                ..
            }
        ));
    }

    // ===== #589/#590: control-char salvage + object-shape normalization =====
    #[test]
    fn repair_escapes_control_characters_inside_strings() {
        let raw = test_support::CORRUPT_TOOL_ARGS_589;
        let repaired =
            repair_tool_arguments(raw).expect("control characters in strings must be repaired");
        let value: serde_json::Value =
            serde_json::from_str(&repaired).expect("repaired #589 payload must parse");
        assert!(value.is_object(), "salvage must recover an object");
        assert_eq!(
            value["tool_name"], "list_hosts",
            "the intended call must survive the salvage"
        );
    }

    #[test]
    fn repair_leaves_inter_token_whitespace_untouched() {
        let raw = "{\n  \"a\": 1\n}";
        assert!(repair_tool_arguments(raw).is_none());
    }

    #[test]
    fn repair_control_chars_still_does_not_close_truncation() {
        // A payload with an in-string control char AND a truncated tail: the
        // control char is escaped, but the truncation is never closed — the
        // reparse still fails and the incomplete value never runs.
        let raw = "{\"note\":\"line one\nline two that got cut";
        let error = parse_tool_args::<SingleField>(raw)
            .expect_err("truncated payload must not run even after control-char repair");
        assert!(matches!(
            error,
            ToolError::UnparseableArgs {
                kind: UnparseableArgsKind::Malformed,
                ..
            }
        ));
    }

    #[derive(Debug, Deserialize)]
    struct DescribeToolArgs {
        tool_name: String,
    }

    #[test]
    fn parse_tool_args_salvages_589_contamination_to_typed_args() {
        let parsed: DescribeToolArgs = parse_tool_args(test_support::CORRUPT_TOOL_ARGS_589)
            .expect("the #589 payload must salvage into typed args");
        assert_eq!(parsed.tool_name, "list_hosts");
    }

    fn normalize(value: &serde_json::Value) -> serde_json::Value {
        normalize_tool_call_arguments("test", "echo", value)
    }

    #[test]
    fn normalize_passes_object_through_unchanged() {
        let object = serde_json::json!({"city": "NYC", "nested": {"a": [1, 2]}});
        assert_eq!(normalize(&object), object);
    }

    #[test]
    fn normalize_coerces_null_and_empty_string_to_empty_object() {
        assert_eq!(normalize(&serde_json::Value::Null), serde_json::json!({}));
        assert_eq!(normalize(&serde_json::json!("")), serde_json::json!({}));
        assert_eq!(
            normalize(&serde_json::json!("  \n ")),
            serde_json::json!({})
        );
    }

    #[test]
    fn normalize_parses_stringified_object() {
        assert_eq!(
            normalize(&serde_json::json!("{\"city\":\"NYC\"}")),
            serde_json::json!({"city": "NYC"})
        );
    }

    #[test]
    fn normalize_salvages_589_corrupt_string_payload() {
        let poison = serde_json::Value::String(test_support::CORRUPT_TOOL_ARGS_589.into());
        let normalized = normalize(&poison);
        assert!(normalized.is_object());
        assert_eq!(normalized["tool_name"], "list_hosts");
    }

    #[test]
    fn normalize_coerces_non_object_shapes_to_empty_object() {
        // Full #590 reproduction matrix: "[]" (the repro), a JSON string
        // literal (the production case), scalars, arrays.
        for poison in [
            serde_json::json!("[]"),
            serde_json::json!("[1,2]"),
            serde_json::json!("123"),
            serde_json::json!("true"),
            serde_json::json!("null"),
            serde_json::json!("\"any string\""),
            serde_json::json!("not json at all"),
            serde_json::json!("{\"a\":"), // truncated
            serde_json::json!([]),
            serde_json::json!([1, 2]),
            serde_json::json!(123),
            serde_json::json!(true),
        ] {
            assert_eq!(
                normalize(&poison),
                serde_json::json!({}),
                "non-object arguments {poison:?} must coerce to {{}}"
            );
        }
    }

    #[test]
    fn normalize_is_idempotent() {
        for value in [
            serde_json::json!({"city": "NYC"}),
            serde_json::json!("[]"),
            serde_json::json!("{\"a\": 1}"),
            serde_json::Value::Null,
            serde_json::json!([1]),
            serde_json::Value::String(test_support::CORRUPT_TOOL_ARGS_589.into()),
        ] {
            let once = normalize(&value);
            assert_eq!(normalize(&once), once, "normalize must be idempotent");
        }
    }

    #[test]
    fn parse_tool_args_lone_backslash_then_truncation_is_not_run() {
        // Escape repair must never complete a half-written value.
        let raw = r#"{"report_type":"steward C:\drive","body":"the cluster is healthy, node C is"#;
        let first = serde_json::from_str::<Sample>(raw).expect_err("must not parse");
        assert_ne!(first.classify(), serde_json::error::Category::Eof);
        let error = parse_tool_args::<Sample>(raw)
            .expect_err("malformed-early + truncated-late must NOT run");
        assert!(
            matches!(
                error,
                ToolError::UnparseableArgs {
                    kind: UnparseableArgsKind::Malformed,
                    ..
                }
            ),
            "post-escape reparse remains malformed without a provider reason, got: {error:?}"
        );
    }
}
