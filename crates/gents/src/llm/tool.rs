use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};

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
    /// deserialize) or truncated by the generation hitting its token cap
    /// (`finish_reason == "length"`, surfaced here as a [`serde_json`]
    /// `Category::Eof`). The dispatcher renders this into a `JsonError:`-prefixed
    /// tool result so the call terminalizes `failed(ArgumentInvalid)` and the
    /// model is told what went wrong (truncated vs malformed) and re-emits a
    /// corrected tool call on its next turn — rather than the parse failure being
    /// swallowed as a generic result the model blindly repeats.
    #[error("tool args unparseable ({kind}): {reason}")]
    UnparseableArgs {
        kind: UnparseableArgsKind,
        reason: String,
    },
    /// A trusted tool adapter observed an execution failure and rendered the
    /// model-facing detail without asking persistence to infer semantics from
    /// arbitrary output text.
    #[error("{text}")]
    ReportedFailure { class: FailureClass, text: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnparseableArgsKind {
    Truncated,
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

pub trait Tool: Sized + Send + Sync {
    const NAME: &'static str;
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

    /// Convert the concrete tool error at the trusted adapter boundary. Tools
    /// that deliberately return a recoverable, model-facing failure override
    /// this to preserve its typed class and rendered detail.
    fn into_dyn_error(error: Self::Error) -> ToolError {
        ToolError::ToolCallError(Box::new(error))
    }
}

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
                .map_err(<Self as Tool>::into_dyn_error)
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
pub(crate) fn parse_tool_args<A>(args: &str) -> Result<A, ToolError>
where
    A: for<'de> Deserialize<'de>,
{
    let first_error = match serde_json::from_str::<A>(args) {
        Ok(value) => return Ok(value),
        Err(error) => error,
    };

    // Attempt one tolerant repair pass, then re-parse into `Args`. The repair is
    // deliberately ESCAPE-ONLY: it doubles lone backslashes the model emitted raw
    // (`\d`, `C:\temp`) but does NOT close a truncated value. That is the safety
    // property — a payload cut mid-value (`finish_reason == "length"`) can never
    // be "completed" by the repair into something that deserializes, so a
    // truncated value is never run; it always falls through to the typed error
    // below. (An earlier version also closed dangling strings/brackets, which let
    // a value truncated inside its last field deserialize and run — a silent
    // half-written commit. Escape-only repair removes that class entirely.)
    if let Some(repaired) = repair_tool_arguments(args) {
        match serde_json::from_str::<A>(&repaired) {
            Ok(value) => return Ok(value),
            Err(second_error) => return Err(unparseable_args_error(&second_error)),
        }
    }

    Err(unparseable_args_error(&first_error))
}

/// Map a failing [`serde_json::Error`] to the typed [`ToolError::UnparseableArgs`].
/// A `Category::Eof` failure is the parse-seam fingerprint of a
/// `finish_reason == "length"` truncation (the generation hit its token cap
/// mid-arguments); everything else is treated as malformed.
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
/// repair was applied. Two corruptions are handled, both escapes:
///
/// 1. **Lone backslashes**: the model writes a single backslash that is not a
///    legal JSON escape (`\d`, `C:\temp`, a bare `\` before a normal char). We
///    walk the string and double any backslash that does not introduce a valid
///    JSON escape (`\" \\ \/ \b \f \n \r \t \uXXXX`), turning the raw output
///    into the escaped form the model should have emitted.
/// 2. **Literal control characters inside string literals** (#589): the
///    model's reasoning channel bleeds raw newlines (and other `0x00-0x1f`
///    bytes) into the middle of a JSON string, which `serde_json` rejects as
///    "control character found while parsing a string". We escape them
///    (`\n`, `\r`, `\t`, … `\uXXXX`), preserving the model's intent — a
///    literal control character inside a string is never legal JSON, so the
///    transform is identity on valid payloads. Control characters BETWEEN
///    tokens are legal whitespace and are left untouched. This pass runs
///    after backslash-doubling so every backslash begins a well-formed escape
///    and in-string tracking is unambiguous.
///
/// The repair is **escape-only by design**: it never closes a truncated value
/// (dangling string / unbalanced brackets). Closing a truncation can produce JSON
/// that deserializes even though the content was cut mid-field, which would let a
/// half-written value (e.g. a truncated status report) run as if it were whole.
/// By refusing to close, a `finish_reason == "length"` payload simply stays
/// unparseable and is surfaced as [`ToolError::UnparseableArgs`] instead of run.
///
/// Returns `None` when neither corruption was found (the caller has already
/// tried a clean parse, so there is nothing to gain from re-parsing an
/// identical string).
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
/// shape (Lean `PromptAssembly.normalizeArgs`, issues #589/#590). Providers
/// render history through templates that iterate `arguments.items()`, so a
/// non-object value — a raw `Value::String` (the #589 poison), an array, a
/// scalar, or `null` — deterministically jams every subsequent render of the
/// session (#590). Applied at both rig-converter seams
/// ([`crate::llm::rig_compat::from_rig_tool_call`], ingest — nothing
/// non-object is ever accumulated into durable history — and
/// [`crate::llm::rig_compat::to_rig_tool_call`], egress — already-poisoned
/// durable history self-heals at request build).
///
/// The policy (each case fenced by conformance vectors mirroring the Lean
/// theorems N1–N4):
/// - an object passes through **unchanged** (N2);
/// - `null` and an empty/whitespace string become `{}` silently (the
///   absent-args shape providers accept);
/// - a string that parses — after the tolerant escape-only
///   [`repair_tool_arguments`] pass — to an object becomes **that object**
///   (N4: the intended call survives, e.g. the #589 corrupt payload);
/// - anything else (non-object JSON, unparseable string, array, scalar)
///   becomes `{}` with a bounded warning so production occurrences are
///   countable without dumping unbounded payloads.
///
/// `seam` labels the boundary ("ingest"/"egress") in the warning so healing
/// of old poison is distinguishable from newly ingested poison.
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
        // A mix in a COMPLETE object: legal `\n` and `\"` stay; the raw `\d` gets
        // doubled. The result must parse and keep the legal escapes' meaning.
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
        // `é` is a legal escape and must not be doubled.
        let raw = r#"{"body":"café \d"}"#;
        let repaired = repair_tool_arguments(raw).expect("the lone \\d should trigger a repair");
        let value: serde_json::Value =
            serde_json::from_str(&repaired).expect("repaired payload must parse");
        assert_eq!(value["body"], "café \\d");
    }

    #[test]
    fn repair_does_not_close_truncation() {
        // Escape-only repair never completes a cut-off value. A payload truncated
        // mid-string has no lone backslash to fix, so repair is a no-op (None) and
        // the truncation survives to be reported — never closed-and-run.
        let raw = r#"{"report_type":"steward","body":"partial body that got cut"#;
        assert!(
            repair_tool_arguments(raw).is_none(),
            "truncation must not be repaired into valid JSON"
        );
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
    fn parse_tool_args_truncated_reports_truncated_kind() {
        // Truncated mid-string: escape-only repair cannot complete it, so we
        // surface the typed signal kinded Truncated (finish_reason=length / serde
        // Category::Eof).
        let raw = r#"{"report_type":"steward","body":"a long body that got cut o"#;
        let error = parse_tool_args::<Sample>(raw).expect_err("truncated payload must not parse");
        assert!(matches!(
            error,
            ToolError::UnparseableArgs {
                kind: UnparseableArgsKind::Truncated,
                ..
            }
        ));
    }

    #[test]
    fn parse_tool_args_malformed_reports_malformed_kind() {
        // A complete-but-malformed object the repair cannot make deserialize into
        // the typed args (here `findings` is a string, not an array — a non-Eof,
        // non-repairable shape) is classified Malformed.
        let raw = r#"{"report_type":"steward","body":"ok","findings":"not-an-array"}"#;
        let error =
            parse_tool_args::<Sample>(raw).expect_err("type-mismatched payload must not parse");
        assert!(matches!(
            error,
            ToolError::UnparseableArgs {
                kind: UnparseableArgsKind::Malformed,
                ..
            }
        ));
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
        // runs; it is reported Truncated.
        let raw = r#"{"note":"a long note that got cut o"#;
        let error = parse_tool_args::<SingleField>(raw)
            .expect_err("a truncated-but-would-type-check payload must NOT run");
        assert!(matches!(
            error,
            ToolError::UnparseableArgs {
                kind: UnparseableArgsKind::Truncated,
                ..
            }
        ));
    }

    // ===== #589/#590: control-char salvage + object-shape normalization =====

    #[test]
    fn repair_escapes_control_characters_inside_strings() {
        // The #589 production class: literal newlines (control characters)
        // inside JSON strings, from reasoning-channel bleed. Escaping them is
        // semantics-preserving (the model meant a newline) and lets the
        // intended object parse; duplicate keys resolve last-wins.
        let raw = crate::test_support::CORRUPT_TOOL_ARGS_589;
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
        // Newlines BETWEEN tokens are legal JSON whitespace; a payload that is
        // already valid must not be "repaired" (None = nothing to re-parse).
        let raw = "{\n  \"a\": 1\n}";
        assert!(repair_tool_arguments(raw).is_none());
    }

    #[test]
    fn repair_control_chars_still_does_not_close_truncation() {
        // A payload with an in-string control char AND a truncated tail: the
        // control char is escaped, but the truncation is never closed — the
        // reparse still fails Eof and is reported Truncated, never run (#512
        // guarantee preserved).
        let raw = "{\"note\":\"line one\nline two that got cut";
        let error = parse_tool_args::<SingleField>(raw)
            .expect_err("truncated payload must not run even after control-char repair");
        assert!(matches!(
            error,
            ToolError::UnparseableArgs {
                kind: UnparseableArgsKind::Truncated,
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
        // Dispatch-level salvage: the corrupt payload deserializes into the
        // tool's typed Args after repair, so the intended call runs instead of
        // wasting a turn on "re-call the tool with valid JSON".
        let parsed: DescribeToolArgs = parse_tool_args(crate::test_support::CORRUPT_TOOL_ARGS_589)
            .expect("the #589 payload must salvage into typed args");
        assert_eq!(parsed.tool_name, "list_hosts");
    }

    fn normalize(value: &serde_json::Value) -> serde_json::Value {
        normalize_tool_call_arguments("test", "echo", value)
    }

    #[test]
    fn normalize_passes_object_through_unchanged() {
        // N2 (object fixpoint): the healthy flow has no regression.
        let object = serde_json::json!({"city": "NYC", "nested": {"a": [1, 2]}});
        assert_eq!(normalize(&object), object);
    }

    #[test]
    fn normalize_coerces_null_and_empty_string_to_empty_object() {
        // Empty/absent arguments egress as {} (the 200-control shape).
        assert_eq!(normalize(&serde_json::Value::Null), serde_json::json!({}));
        assert_eq!(normalize(&serde_json::json!("")), serde_json::json!({}));
        assert_eq!(
            normalize(&serde_json::json!("  \n ")),
            serde_json::json!({})
        );
    }

    #[test]
    fn normalize_parses_stringified_object() {
        // N4 (salvage): a JSON string holding an object becomes that object.
        assert_eq!(
            normalize(&serde_json::json!("{\"city\":\"NYC\"}")),
            serde_json::json!({"city": "NYC"})
        );
    }

    #[test]
    fn normalize_salvages_589_corrupt_string_payload() {
        // N4 on the production poison: the persisted Value::String of corrupt
        // bytes normalizes to the intended object, so a poisoned session
        // self-heals on next egress.
        let poison = serde_json::Value::String(crate::test_support::CORRUPT_TOOL_ARGS_589.into());
        let normalized = normalize(&poison);
        assert!(normalized.is_object());
        assert_eq!(normalized["tool_name"], "list_hosts");
    }

    #[test]
    fn normalize_coerces_non_object_shapes_to_empty_object() {
        // N1 (soundness): every non-object, non-salvageable shape becomes {}.
        // Covers the full #590 reproduction matrix: "[]" (the repro), a JSON
        // string literal (the production case), scalars, arrays.
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
        // N3: ingest-then-egress normalization composes without drift.
        for value in [
            serde_json::json!({"city": "NYC"}),
            serde_json::json!("[]"),
            serde_json::json!("{\"a\": 1}"),
            serde_json::Value::Null,
            serde_json::json!([1]),
            serde_json::Value::String(crate::test_support::CORRUPT_TOOL_ARGS_589.into()),
        ] {
            let once = normalize(&value);
            assert_eq!(normalize(&once), once, "normalize must be idempotent");
        }
    }

    #[test]
    fn parse_tool_args_lone_backslash_then_truncation_is_not_run() {
        // The corner the first ultracode review caught: a lone-backslash escape
        // EARLY (so serde's first error is Syntax, not Eof) and truncation LATE.
        // An escape-and-close repair would fix the backslash, close the cut, and
        // run a half-written body. Escape-only repair fixes the backslash but
        // leaves the truncation, so the reparse fails with Eof and we report
        // Truncated — the truncated value is never run.
        let raw = r#"{"report_type":"steward C:\drive","body":"the cluster is healthy, node C is"#;
        // The early lone backslash means serde's FIRST error is a syntax error.
        let first = serde_json::from_str::<Sample>(raw).expect_err("must not parse");
        assert_ne!(first.classify(), serde_json::error::Category::Eof);
        let error = parse_tool_args::<Sample>(raw)
            .expect_err("malformed-early + truncated-late must NOT run");
        assert!(
            matches!(
                error,
                ToolError::UnparseableArgs {
                    kind: UnparseableArgsKind::Truncated,
                    ..
                }
            ),
            "post-escape reparse fails with Eof, so it is reported Truncated, got: {error:?}"
        );
    }
}
