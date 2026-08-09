use serde_json::json;

use super::spill::{extract_mutation_doc_id, resolve_exact_replay};
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

#[test]
fn exact_result_replay_requires_one_identical_physical_fact() {
    let exact = vec![("doc-a", true)];
    let replay = resolve_exact_replay(exact, |row| row.0, |row| row.1)
        .expect("one exact fact is an idempotent replay")
        .expect("the exact fact should be adopted");
    assert_eq!(replay.0, "doc-a");

    let conflict = resolve_exact_replay(vec![("doc-a", false)], |row| row.0, |row| row.1)
        .expect_err("a conflicting fact must block another publication");
    assert!(conflict.to_string().contains("conflicts"));

    let twins = resolve_exact_replay(
        vec![("doc-b", true), ("doc-a", true)],
        |row| row.0,
        |row| row.1,
    )
    .expect_err("identical twins still make physical provenance ambiguous");
    assert!(twins.to_string().contains("2 physical facts"));
}

#[test]
fn exact_output_contract_reconstructs_bounded_projection_and_spill_pointer() {
    let output = (0..5)
        .map(|index| format!("line-{index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let metadata = json!({
        "projection_version": 1,
        "truncated": true,
        "truncated_by": "lines",
        "mode": "head",
        "original_lines": 5,
        "original_bytes": output.len(),
        "max_lines": 2,
        "max_bytes": 1024,
        "spill_reference": true,
    })
    .to_string();

    let projection = super::canonical_model_projection(&output, "result-doc", true, &metadata)
        .expect("signed projection contract should reproduce");
    assert!(projection.starts_with("line-0\nline-1\n\n[Showing lines 1-2 of 5"));
    assert!(projection.ends_with("[Full output: DefraDB doc result-doc]"));
    assert!(!projection.contains("line-4"));
}

#[test]
fn exact_output_contract_rejects_forged_truncation_claim() {
    let output = "one\ntwo\nthree";
    let metadata = json!({
        "projection_version": 1,
        "truncated": true,
        "truncated_by": "bytes",
        "mode": "head",
        "original_lines": 3,
        "original_bytes": output.len(),
        "max_lines": 2,
        "max_bytes": 1024,
        "spill_reference": true,
    })
    .to_string();

    let error = super::canonical_model_projection(output, "result-doc", true, &metadata)
        .expect_err("metadata cannot relabel a line truncation as byte truncation");
    assert!(error.to_string().contains("does not reproduce"));
}

#[test]
fn exact_output_contract_rejects_limits_above_provider_ceiling() {
    let output = "small";
    let metadata = json!({
        "projection_version": 1,
        "truncated": false,
        "truncated_by": null,
        "mode": "head",
        "original_lines": 1,
        "original_bytes": output.len(),
        "max_lines": TruncationLimits::default().max_lines + 1,
        "max_bytes": TruncationLimits::default().max_bytes,
        "spill_reference": false,
    })
    .to_string();

    let error = super::canonical_model_projection(output, "result-doc", false, &metadata)
        .expect_err("signed metadata cannot expand the provider boundary");
    assert!(error.to_string().contains("exceeds provider input limits"));
}

#[test]
fn projection_writer_rejects_unreadable_v1_limits() {
    for limits in [
        TruncationLimits {
            max_lines: 0,
            max_bytes: TruncationLimits::default().max_bytes,
        },
        TruncationLimits {
            max_lines: TruncationLimits::default().max_lines,
            max_bytes: 0,
        },
        TruncationLimits {
            max_lines: TruncationLimits::default().max_lines + 1,
            max_bytes: TruncationLimits::default().max_bytes,
        },
        TruncationLimits {
            max_lines: TruncationLimits::default().max_lines,
            max_bytes: TruncationLimits::default().max_bytes + 1,
        },
    ] {
        let error = super::validate_model_projection_limits(&limits)
            .expect_err("writers cannot publish a v1 fact that readers reject");
        assert!(error.to_string().contains("exceeds provider input limits"));
    }
}

#[test]
fn projection_publication_revalidates_caller_supplied_metadata() {
    let output = "small";
    let metadata = json!({
        "projection_version": 1,
        "truncated": false,
        "truncated_by": null,
        "mode": "head",
        "original_lines": 1,
        "original_bytes": output.len(),
        "max_lines": TruncationLimits::default().max_lines + 1,
        "max_bytes": TruncationLimits::default().max_bytes,
        "spill_reference": true,
    })
    .to_string();

    let error = super::validate_model_projection_metadata(output, false, &metadata)
        .expect_err("direct retention cannot persist unreadable caller metadata");
    assert!(error.to_string().contains("exceeds provider input limits"));
}

#[test]
fn exact_output_contract_requires_its_result_document_pointer() {
    let output = "small";
    let metadata = json!({
        "projection_version": 1,
        "truncated": false,
        "truncated_by": null,
        "mode": "head",
        "original_lines": 1,
        "original_bytes": output.len(),
        "max_lines": TruncationLimits::default().max_lines,
        "max_bytes": TruncationLimits::default().max_bytes,
        "spill_reference": false,
    })
    .to_string();

    let error = super::canonical_model_projection(output, "result-doc", false, &metadata)
        .expect_err("an exact output fact cannot omit its paging authority");
    assert!(error
        .to_string()
        .contains("omits its exact result document"));
}

#[test]
fn untruncated_output_retains_pointer_authority_without_rendering_it() {
    let output = "small";
    let limits = TruncationLimits::default();
    let metadata = super::model_projection_metadata(output, super::TruncationMode::Head, &limits);
    let contract: serde_json::Value =
        serde_json::from_str(&metadata).expect("writer metadata must be valid JSON");
    assert_eq!(contract["truncated"].as_bool(), Some(false));
    assert_eq!(contract["spill_reference"].as_bool(), Some(true));

    let projection = super::canonical_model_projection(output, "result-doc", false, &metadata)
        .expect("writer metadata must authorize one canonical projection");
    assert_eq!(projection, output);
    assert!(!projection.contains("[Full output: DefraDB doc"));

    let error = super::canonical_model_projection(output, "", false, &metadata)
        .expect_err("lossless projections still require exact pointer authority");
    assert!(error
        .to_string()
        .contains("requires an exact result document"));
}

#[test]
fn lean_projection_cases_drive_the_production_canonical_boundary() {
    let output = (0..5)
        .map(|index| format!("line-{index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let preserved_full_output = output.clone();
    let limits = TruncationLimits {
        max_lines: 2,
        max_bytes: 1024,
    };
    let metadata = model_projection_metadata(&output, TruncationMode::Head, &limits);
    let canonical = canonical_model_projection(&output, "result-doc", true, &metadata)
        .expect("production projection contract must reproduce");

    let cases = crate::lean_vocab_test::lean_tool_output_projection_cases();
    assert_eq!(
        cases.len(),
        3,
        "Lean must cover every projection observation"
    );
    for case in cases {
        let observed = match case.observation.as_str() {
            "canonical" => canonical.clone(),
            "full_output" => output.clone(),
            "forged" => format!("{canonical}-forged"),
            other => panic!("unknown Lean projection observation {other}"),
        };
        assert_eq!(
            observed == canonical,
            case.accepted,
            "Lean/production projection acceptance diverged for {}",
            case.name
        );
        assert_eq!(
            output == preserved_full_output,
            case.full_output_preserved,
            "projection evaluation altered the full output for {}",
            case.name
        );
    }
}
