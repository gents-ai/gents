//! `gents eval checks`: the builtin catalog, from the registry alone.

use std::io::Write;

use anyhow::{Context, Result};
use gents::document_config::EvalCase;
use gents::eval::checks::CheckRegistry;

use super::init::validate::case_catalog_conformance;
use crate::cli::EvalChecksArgs;

pub(super) fn checks(
    registry: &CheckRegistry,
    args: &EvalChecksArgs,
    out: &mut dyn Write,
) -> Result<()> {
    if let Some(source) = &args.validate_case {
        return validate_case(registry, &read_case(source)?, out);
    }
    let catalog = registry.catalog();
    if args.json {
        return super::write_json(out, &catalog);
    }
    for check in &catalog {
        writeln!(out, "{} v{}  {}", check.name, check.version, check.summary)?;
        writeln!(
            out,
            "  params  {}",
            serde_json::to_string(&check.params_schema)?
        )?;
        writeln!(out, "  reads   {}", check.reads.join(", "))?;
        for (code, line) in &check.reason_codes {
            writeln!(out, "  reason  {code}: {line}")?;
        }
    }
    Ok(())
}

fn read_case(source: &str) -> Result<String> {
    if source == "-" {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)
            .context("reading the case from stdin")?;
        return Ok(text);
    }
    std::fs::read_to_string(source).with_context(|| format!("reading {source}"))
}

/// Prints `{"violations": [...]}` for one case's JSON text and fails when it
/// holds any, so a pack's contract test can pipe a case in and trust the exit
/// status.
fn validate_case(registry: &CheckRegistry, text: &str, out: &mut dyn Write) -> Result<()> {
    let case: EvalCase = serde_json::from_str(text).context("parsing the eval case")?;
    let violations: Vec<String> = case_catalog_conformance(&case, registry)
        .into_iter()
        .collect();
    super::write_json(out, &serde_json::json!({ "violations": violations }))?;
    anyhow::ensure!(
        violations.is_empty(),
        "the case names {} check problem(s); fix them and retry",
        violations.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use gents::eval::checks::CheckDescription;

    use super::*;

    #[test]
    fn json_output_is_the_builtin_catalog() {
        let registry = CheckRegistry::builtin();
        let args = EvalChecksArgs {
            json: true,
            validate_case: None,
        };
        let mut out = Vec::new();
        checks(&registry, &args, &mut out).unwrap();
        let parsed: Vec<CheckDescription> = serde_json::from_slice(&out).unwrap();
        assert_eq!(parsed, registry.catalog());
    }

    fn case_text(check: &str, params: serde_json::Value) -> String {
        serde_json::json!({
            "case_id": "c1",
            "split": "validation",
            "stages": [{
                "stage_id": "s1",
                "prompt": "Run it.",
                "deadline_secs": 60,
                "checks": [{"check": check, "params": params, "tier": "acceptance"}],
            }],
        })
        .to_string()
    }

    #[test]
    fn validate_case_accepts_a_registered_check_with_valid_params() {
        let mut out = Vec::new();
        let text = case_text(
            "captured_rows_count",
            serde_json::json!({"name": "findings", "min": 1}),
        );
        validate_case(&CheckRegistry::builtin(), &text, &mut out).unwrap();
        let printed: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(printed, serde_json::json!({"violations": []}));
    }

    #[test]
    fn validate_case_reports_unknown_checks_and_bad_params() {
        let registry = CheckRegistry::builtin();
        let mut out = Vec::new();
        let error = validate_case(
            &registry,
            &case_text("no_such", serde_json::json!({})),
            &mut out,
        )
        .unwrap_err();
        assert!(error.to_string().contains("1 check problem"), "{error:#}");
        let printed: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert!(
            printed["violations"][0]
                .as_str()
                .is_some_and(|line| line.contains("unknown check \"no_such\"")),
            "{printed}"
        );

        let mut out = Vec::new();
        let bad = case_text("captured_rows_count", serde_json::json!({"name": 7}));
        validate_case(&registry, &bad, &mut out).unwrap_err();
        let printed: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert!(
            printed["violations"][0]
                .as_str()
                .is_some_and(|line| line.contains("captured_rows_count params")),
            "{printed}"
        );
    }
}
