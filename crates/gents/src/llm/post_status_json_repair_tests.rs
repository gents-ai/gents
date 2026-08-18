//! Steward `post_status` tool-call argument deserialization on large markdown
//! bodies (single-backslash escapes / embedded newlines / backticks / mid-object
//! truncation).
//!
//! ## What this pins
//!
//! Stewards post their status report through a tool whose `Args` carry the
//! report body as a JSON string and the per-finding details as a JSON array of
//! strings (the `findings` array-param shape called out in the d4f raw-escape
//! memory). When the model emits a ~5–8 KB markdown body that contains
//! single-backslash sequences (`\d+`, Windows paths `C:\temp`), embedded
//! newlines, and backticks, the tool-call `arguments` string that reaches the
//! client either:
//!
//!   * contains an INVALID JSON ESCAPE (the model wrote a single backslash that
//!     is not a legal JSON escape), or
//!   * is TRUNCATED mid-object (the generation hit a token cap before closing
//!     the object — `finish_reason == "length"`).
//!
//! Both land at the SAME client seam: `crate::llm::tool::ToolDyn::call`
//! (`crates/gents/src/llm/tool.rs`), which deserializes the model's
//! `arguments` string into the tool's typed `Args`.
//!
//! Before the fix, a parse failure there became `ToolError::JsonError`, which the
//! dispatcher (`agent/loop_stream.rs`) turned into the *tool result string* fed
//! back to the model — so the model just saw a raw error and re-emitted the same
//! oversized payload until the budget was spent, and NOTHING was posted.
//!
//! After the fix, `ToolDyn::call` attempts an ESCAPE-ONLY repair (double raw lone
//! backslashes; it never closes a truncated value) and re-parses:
//!
//!   * The single-backslash case is REPAIRED and the tool runs (the report
//!     posts) — see [`post_status_json_single_backslash_escape_recovers_and_posts`].
//!   * The truncation case can NOT be completed by an escape-only repair, so it
//!     is reported as `ToolError::UnparseableArgs { kind: Truncated, .. }` and is
//!     never run — see [`post_status_json_truncated_body_is_rejected_not_posted`]
//!     and [`post_status_json_truncation_signal_is_offset_independent`]. The
//!     dispatcher then renders that into a clean parse-failure notice for the
//!     model (terminalizing the call `failed(ArgumentInvalid)`) so the model
//!     re-emits corrected arguments on its next turn — fail-fast, not a hidden
//!     daemon retry.
//!
//! Note the error TEXT on the truncation path: Rust's `serde_json` reports
//! `Category::Eof` ("EOF while parsing ..."). That is the CLIENT parser, distinct
//! from vLLM's server-side Python `json` parser (handled separately in
//! `crate::error::classify_completion_error`). This file pins the CLIENT-side
//! variant.
//!
//! These tests are deterministic and need no live backend; they exercise the
//! real `ToolDyn::call` seam directly.

use crate::llm::tool::{Tool, ToolDefinition, ToolDyn, ToolError, UnparseableArgsKind};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
struct PostStatusArgs {
    #[allow(dead_code)]
    report_type: String,
    #[allow(dead_code)]
    scope: String,
    #[allow(dead_code)]
    body: String,
    #[allow(dead_code)]
    findings: Vec<String>,
}

#[derive(Debug, Serialize)]
struct PostStatusOutput {
    posted: bool,
}

#[derive(Debug, thiserror::Error)]
#[error("post_status tool failure: {0}")]
struct PostStatusError(String);

struct PostStatusTool;

impl Tool for PostStatusTool {
    const NAME: &'static str = "post_status";
    type Error = PostStatusError;
    type Args = PostStatusArgs;
    type Output = PostStatusOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Post a steward status report.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "report_type": {"type": "string"},
                    "scope": {"type": "string"},
                    "body": {"type": "string"},
                    "findings": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["report_type", "scope", "body", "findings"]
            }),
        }
    }

    async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
        Ok(PostStatusOutput { posted: true })
    }
}

/// A realistic ~5–8 KB steward markdown body: embedded newlines, backticks,
/// fenced code blocks, and SINGLE-backslash sequences (regex + Windows/UNC
/// paths) that are NOT legal JSON string escapes.
fn large_steward_markdown_body() -> String {
    let mut body = String::new();
    body.push_str("# Steward Status — host-check\n\n");
    body.push_str("## Summary\n\nUpdate surface scan complete.\n\n");
    for i in 0..40 {
        body.push_str(&format!("### Finding {i}\n\n"));
        body.push_str("Matched the quarantine regex `\\d+\\.\\d+` against ");
        body.push_str("the path `C:\\Program Files\\vendor\\agent` and the ");
        body.push_str("UNC share `\\\\nas\\share\\drop`.\n\n");
        body.push_str("```sh\ngrep -E '\\bv\\d+\\b' /var/log/host.log\n```\n\n");
        body.push_str("Notes: escaped tab \\t and newline \\n appear literally ");
        body.push_str("in the operator's pasted command line.\n\n");
    }
    assert!(
        body.len() >= 5_000,
        "body should be a realistic large report (~5-8 KB), got {} bytes",
        body.len()
    );
    body
}

fn arguments_with_single_backslash_escape() -> String {
    let valid = serde_json::json!({
        "report_type": "steward",
        "scope": "host:studio-2",
        "body": large_steward_markdown_body(),
        "findings": [
            "regex \\d+ matched on C:\\temp\\foo",
            "path \\\\nas\\share unreachable"
        ]
    })
    .to_string();
    valid.replace("\\\\", "\\")
}

fn arguments_truncated_mid_object(cut_at: usize) -> String {
    let valid = serde_json::json!({
        "report_type": "steward",
        "scope": "host:studio-2",
        "body": large_steward_markdown_body(),
        "findings": ["first finding", "second finding"]
    })
    .to_string();
    let mut end = cut_at.min(valid.len());
    while !valid.is_char_boundary(end) {
        end -= 1;
    }
    valid[..end].to_string()
}

#[tokio::test]
async fn post_status_json_single_backslash_escape_recovers_and_posts() {
    let args = arguments_with_single_backslash_escape();
    assert!(
        serde_json::from_str::<serde_json::Value>(&args).is_err(),
        "the corrupted single-backslash payload should be invalid JSON pre-repair"
    );
    let tool = PostStatusTool;

    let output = ToolDyn::call(&tool, args)
        .await
        .expect("a single-backslash escape in a large post_status body should be repaired client-side and the tool should run");
    assert!(
        output.contains("posted") || output.contains("true"),
        "expected the post_status tool to execute and report success, got: {output}"
    );
}

#[tokio::test]
async fn post_status_json_truncated_body_is_rejected_not_posted() {
    let args = arguments_truncated_mid_object(5_037);
    let tool = PostStatusTool;

    let error = ToolDyn::call(&tool, args)
        .await
        .expect_err("a truncated post_status arguments string must NOT post; it must be rejected");

    match error {
        ToolError::UnparseableArgs { kind, reason } => {
            assert_eq!(
                kind,
                UnparseableArgsKind::Truncated,
                "a mid-object cut is the finish_reason=length shape (serde Category::Eof)"
            );
            assert!(
                reason.contains("EOF while parsing"),
                "diagnostic: expected a serde_json EOF reason (client parser), got: {reason}"
            );
        }
        other => {
            panic!("expected ToolError::UnparseableArgs {{ Truncated }}, got: {other:?}")
        }
    }
}

#[tokio::test]
async fn post_status_json_truncation_signal_is_offset_independent() {
    let tool = PostStatusTool;
    for cut_at in [4_096usize, 5_037, 6_500, 8_000] {
        let args = arguments_truncated_mid_object(cut_at);
        let error = ToolDyn::call(&tool, args)
            .await
            .err()
            .unwrap_or_else(|| panic!("cut at {cut_at} should error, not post"));
        match error {
            ToolError::UnparseableArgs { kind, .. } => assert_eq!(
                kind,
                UnparseableArgsKind::Truncated,
                "cut at {cut_at} should be the truncation (Eof) shape"
            ),
            other => panic!("cut at {cut_at}: expected UnparseableArgs, got {other:?}"),
        }
    }
}
