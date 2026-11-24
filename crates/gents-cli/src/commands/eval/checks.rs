use std::io::Write;

use anyhow::{Context, Result};
use gents_documents::EvalCase;
use gents_eval::checks::stage_fields::stage_fields;
use gents_eval::checks::CheckRegistry;

use crate::cli::EvalChecksArgs;
use gents_eval::authoring::validate::case_catalog_conformance;

pub(super) fn checks(
    registry: &CheckRegistry,
    args: &EvalChecksArgs,
    out: &mut dyn Write,
) -> Result<()> {
    if let Some(source) = &args.validate_case {
        return validate_case(registry, &read_case(source)?, out);
    }
    let catalog = registry.catalog();
    let stage_fields = stage_fields();
    if args.json {
        return super::write_json(
            out,
            &serde_json::json!({ "checks": catalog, "stage_fields": stage_fields }),
        );
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
    writeln!(out)?;
    writeln!(out, "Stage fields")?;
    for field in &stage_fields {
        writeln!(out, "{}  ({})", field.name, field.applies_to)?;
        writeln!(out, "  schema  {}", serde_json::to_string(&field.schema)?)?;
        for (code, line) in &field.outcomes {
            writeln!(out, "  {code}: {line}")?;
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
