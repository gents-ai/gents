use serde_json::json;

use super::spill::extract_mutation_doc_id;
use super::*;

#[test]
fn no_truncation_under_limits() {
    let text = "line 1\nline 2\nline 3";
    let (result, trigger, truncated) =
        truncate_text(text, TruncationMode::Head, &TruncationLimits::default());
    assert!(!truncated);
    assert!(trigger.is_none());
    assert_eq!(result, text);
}

#[test]
fn head_truncation_by_lines() {
    let lines: Vec<String> = (0..100).map(|i| format!("line {}", i)).collect();
    let text = lines.join("\n");
    let limits = TruncationLimits {
        max_lines: 10,
        max_bytes: 1024 * 1024,
    };

    let (result, trigger, truncated) = truncate_text(&text, TruncationMode::Head, &limits);
    assert!(truncated);
    assert_eq!(trigger, Some(TruncationTrigger::Lines));
    assert!(result.starts_with("line 0\n"));
    assert!(result.contains("[Showing lines 1-10 of 100"));
}

#[test]
fn tail_truncation_by_lines() {
    let lines: Vec<String> = (0..100).map(|i| format!("line {}", i)).collect();
    let text = lines.join("\n");
    let limits = TruncationLimits {
        max_lines: 10,
        max_bytes: 1024 * 1024,
    };

    let (result, trigger, truncated) = truncate_text(&text, TruncationMode::Tail, &limits);
    assert!(truncated);
    assert_eq!(trigger, Some(TruncationTrigger::Lines));
    assert!(result.contains("line 99"));
    assert!(result.contains("[Showing lines 91-100 of 100"));
}

#[test]
fn head_truncation_by_bytes() {
    let text = "x".repeat(100_000);
    let limits = TruncationLimits {
        max_lines: 1_000_000,
        max_bytes: 1024,
    };

    let (result, trigger, truncated) = truncate_text(&text, TruncationMode::Head, &limits);
    assert!(truncated);
    assert_eq!(trigger, Some(TruncationTrigger::Bytes));
    assert!(result.len() < 100_000);
}

#[test]
fn tail_truncation_by_bytes() {
    let text = "x".repeat(100_000);
    let limits = TruncationLimits {
        max_lines: 1_000_000,
        max_bytes: 1024,
    };

    let (result, trigger, truncated) = truncate_text(&text, TruncationMode::Tail, &limits);
    assert!(truncated);
    assert_eq!(trigger, Some(TruncationTrigger::Bytes));
    assert!(result.len() < 100_000);
}

#[test]
fn both_limits_exceeded() {
    let lines: Vec<String> = (0..5000).map(|i| format!("line {:04}", i)).collect();
    let text = lines.join("\n");
    let limits = TruncationLimits {
        max_lines: 100,
        max_bytes: 1024,
    };

    let (_, trigger, truncated) = truncate_text(&text, TruncationMode::Head, &limits);
    assert!(truncated);
    assert!(trigger.is_some());
}

#[test]
fn extract_mutation_doc_id_accepts_create_and_add_shapes() {
    let create_data = json!({
        "create_AgentToolResult": { "_docID": "doc-create" }
    });
    assert_eq!(
        extract_mutation_doc_id(&create_data, "AgentToolResult"),
        Some("doc-create")
    );

    let add_data = json!({
        "add_AgentToolResult": [{ "_docID": "doc-add" }]
    });
    assert_eq!(
        extract_mutation_doc_id(&add_data, "AgentToolResult"),
        Some("doc-add")
    );
}
