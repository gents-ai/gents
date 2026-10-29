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
    #[error("tool args unparseable ({kind}): {reason}")]
    UnparseableArgs {
        kind: UnparseableArgsKind,
        reason: String,
    },
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

/// A read footprint is a promise from the tool owner that the call has no
/// mutating effects. Non-mutating observation receipts may be deferred. Untrusted tools must use
/// the default barrier, regardless of their advertised metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolFootprint {
    Read,
    Barrier,
}

pub trait Tool: Sized + Send + Sync {
    const NAME: &'static str;
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

    /// Producer receipts travel through the existing dispatch and terminal
    /// owners. Returning them does not commit data or publish a tool terminal.
    fn deferred_writes(
        &self,
        _output: &Self::Output,
    ) -> Option<crate::live_output::DeferredToolWrites> {
        None
    }

    fn footprint(&self, _args: &Self::Args) -> ToolFootprint {
        ToolFootprint::Barrier
    }

    fn admit(&self, _args: &Self::Args) -> Result<(), Self::Error> {
        Ok(())
    }

    fn into_dyn_error(error: Self::Error) -> ToolError {
        ToolError::ToolCallError(Box::new(error))
    }
}

#[derive(Debug)]
pub struct ToolDispatchResult {
    pub result: Result<String, ToolError>,
    pub plugin_receipt: Option<gents_protocol::plugin::PluginExecutionReceipt>,
    /// Writes the tool deferred to the terminal of this call.
    pub deferred_writes: Option<crate::live_output::DeferredToolWrites>,
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
                deferred_writes: None,
            }
        })
    }

    fn footprint(&self, _args: &str) -> ToolFootprint {
        ToolFootprint::Barrier
    }

    fn emits_command_envelope(&self) -> bool {
        false
    }

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

impl<T> ToolDyn for T
where
    T: Tool,
{
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
            <Self as ToolDyn>::call_with_receipt(self, args)
                .await
                .result
        })
    }

    fn call_with_receipt<'a>(&'a self, args: String) -> BoxFuture<'a, ToolDispatchResult> {
        Box::pin(async move {
            let output = async {
                let parsed = parse_tool_args::<<Self as Tool>::Args>(&args)?;
                <Self as Tool>::call(self, parsed)
                    .await
                    .map_err(<Self as Tool>::into_dyn_error)
            }
            .await;
            let (result, deferred_writes) = match output {
                Ok(output) => {
                    let writes = <Self as Tool>::deferred_writes(self, &output);
                    (
                        serialize_tool_output(output).map_err(ToolError::JsonError),
                        writes,
                    )
                }
                Err(error) => (Err(error), None),
            };
            ToolDispatchResult {
                result,
                plugin_receipt: None,
                deferred_writes,
            }
        })
    }

    fn footprint(&self, args: &str) -> ToolFootprint {
        parse_tool_args::<<Self as Tool>::Args>(args)
            .map(|parsed| <Self as Tool>::footprint(self, &parsed))
            .unwrap_or(ToolFootprint::Barrier)
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

pub fn parse_tool_args<A>(args: &str) -> Result<A, ToolError>
where
    A: for<'de> Deserialize<'de>,
{
    let value = match serde_json::from_str::<serde_json::Value>(args) {
        Ok(value) => value,
        Err(first_error) => {
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

/// External premise: `serde_json::Error` exposes only a coarse category
/// (`Data` for every schema mismatch); which mismatch it was exists only in
/// serde's fixed message prefixes (`unknown field`, `missing field`,
/// `invalid type`).
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

/// Bounds provider-supplied argument text included in coercion warning logs.
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

    struct ReceiptTool(std::sync::atomic::AtomicUsize);

    impl Tool for ReceiptTool {
        const NAME: &'static str = "receipt_test";
        type Args = bool;
        type Output = String;
        type Error = std::io::Error;

        async fn definition(&self, _prompt: String) -> ToolDefinition {
            ToolDefinition {
                name: Self::NAME.into(),
                description: String::new(),
                parameters: serde_json::json!({"type": "boolean"}),
            }
        }

        async fn call(&self, succeed: bool) -> Result<String, Self::Error> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if succeed {
                Ok("visible output".into())
            } else {
                Err(std::io::Error::other("failed operation"))
            }
        }

        fn deferred_writes(
            &self,
            _output: &String,
        ) -> Option<crate::live_output::DeferredToolWrites> {
            Some(crate::live_output::DeferredToolWrites::new(17_u64))
        }
    }

    #[tokio::test]
    async fn typed_dispatch_emits_receipts_only_after_one_successful_call() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let tool = ReceiptTool(AtomicUsize::new(0));
        let success = ToolDyn::call_with_receipt(&tool, "true".into()).await;
        assert_eq!(success.result.unwrap(), "visible output");
        assert_eq!(
            success.deferred_writes.unwrap().downcast_ref::<u64>(),
            Some(&17)
        );
        assert!(success.plugin_receipt.is_none());
        assert_eq!(tool.0.load(Ordering::SeqCst), 1);
        let failure = ToolDyn::call_with_receipt(&tool, "false".into()).await;
        assert!(failure.result.is_err());
        assert!(failure.deferred_writes.is_none());
        assert_eq!(tool.0.load(Ordering::SeqCst), 2);
        let invalid = ToolDyn::call_with_receipt(&tool, "{}".into()).await;
        assert!(invalid.result.is_err());
        assert!(invalid.deferred_writes.is_none());
        assert_eq!(tool.0.load(Ordering::SeqCst), 2);
        assert_eq!(
            ToolDyn::call(&tool, "true".into()).await.unwrap(),
            "visible output"
        );
        assert_eq!(tool.0.load(Ordering::SeqCst), 3);
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct Sample {
        report_type: String,
        body: String,
        findings: Vec<String>,
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
    fn repair_does_not_close_truncation() {
        let raw = r#"{"report_type":"steward","body":"partial body that got cut"#;
        assert!(
            repair_tool_arguments(raw).is_none(),
            "truncation must not be repaired into valid JSON"
        );
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

    #[derive(Debug, Deserialize)]
    struct SingleField {
        #[allow(dead_code)]
        note: String,
    }

    #[test]
    fn parse_tool_args_truncation_in_last_field_is_not_run() {
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
    fn normalize_salvages_589_corrupt_string_payload() {
        let poison = serde_json::Value::String(test_support::CORRUPT_TOOL_ARGS_589.into());
        let normalized = normalize(&poison);
        assert!(normalized.is_object());
        assert_eq!(normalized["tool_name"], "list_hosts");
    }

    #[test]
    fn normalize_coerces_non_object_shapes_to_empty_object() {
        for poison in [
            serde_json::json!("[]"),
            serde_json::json!("[1,2]"),
            serde_json::json!("123"),
            serde_json::json!("true"),
            serde_json::json!("null"),
            serde_json::json!("\"any string\""),
            serde_json::json!("not json at all"),
            serde_json::json!("{\"a\":"),
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
