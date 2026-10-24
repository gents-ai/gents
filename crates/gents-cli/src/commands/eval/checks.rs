use std::io::Write;

use gents::document_config::EvalCase;
use gents::eval::checks::CheckRegistry;

use super::init::validate::case_catalog_conformance;
use super::EvalCommandError;
use crate::cli::EvalChecksArgs;

pub(super) fn checks(
    registry: &CheckRegistry,
    args: &EvalChecksArgs,
    out: &mut dyn Write,
) -> Result<(), EvalCommandError> {
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

fn read_case(source: &str) -> Result<String, EvalCommandError> {
    if source == "-" {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)
            .map_err(EvalCommandError::ReadStdin)?;
        return Ok(text);
    }
    std::fs::read_to_string(source).map_err(|source_error| EvalCommandError::ReadCase {
        path: source.to_owned(),
        source: source_error,
    })
}

fn validate_case(
    registry: &CheckRegistry,
    text: &str,
    out: &mut dyn Write,
) -> Result<(), EvalCommandError> {
    let case: EvalCase = serde_json::from_str(text).map_err(EvalCommandError::ParseCase)?;
    let violations: Vec<String> = case_catalog_conformance(&case, registry)
        .into_iter()
        .collect();
    super::write_json(out, &serde_json::json!({ "violations": violations }))?;
    if !violations.is_empty() {
        return Err(EvalCommandError::CaseViolations {
            count: violations.len(),
        });
    }
    Ok(())
}
