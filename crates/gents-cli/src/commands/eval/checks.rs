//! `gents eval checks`: the builtin catalog, from the registry alone.

use std::io::Write;

use anyhow::Result;
use gents::eval::checks::CheckRegistry;

use crate::cli::EvalChecksArgs;

pub(super) fn checks(
    registry: &CheckRegistry,
    args: &EvalChecksArgs,
    out: &mut dyn Write,
) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use gents::eval::checks::CheckDescription;

    use super::*;

    #[test]
    fn json_output_is_the_builtin_catalog() {
        let registry = CheckRegistry::builtin();
        let args = EvalChecksArgs { json: true };
        let mut out = Vec::new();
        checks(&registry, &args, &mut out).unwrap();
        let parsed: Vec<CheckDescription> = serde_json::from_slice(&out).unwrap();
        assert_eq!(parsed, registry.catalog());
    }
}
