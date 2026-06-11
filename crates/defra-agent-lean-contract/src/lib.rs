use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use serde::de::DeserializeOwned;

pub const CONTRACT_JSON_BEGIN: &str = "---BEGIN DEFRA LEAN CONTRACT JSON---";
pub const CONTRACT_JSON_END: &str = "---END DEFRA LEAN CONTRACT JSON---";

// Build only the module artifact needed by `lake env lean --run`; the broader
// target can pull package extra artifacts such as ProofWidgets' widget bundle.
const CONTRACT_TARGET: &str = "Proofs.Conformance.Contracts:olean";
const CONTRACT_RUN_FILE: &str = "Proofs/Conformance/Contracts.lean";

pub fn load_contract_snapshot<T>() -> Result<T>
where
    T: DeserializeOwned,
{
    let stdout = load_contract_stdout()?;
    let json = extract_contract_json(&stdout)?;
    serde_json::from_str(json).with_context(|| {
        format!("failed to parse Lean conformance contract JSON\nstdout:\n{stdout}")
    })
}

pub fn load_contract_json() -> Result<String> {
    let stdout = load_contract_stdout()?;
    extract_contract_json(&stdout).map(ToOwned::to_owned)
}

pub fn load_contract_stdout() -> Result<String> {
    let proofs_dir = proofs_dir()?;
    run_lake_build(&proofs_dir)?;
    run_contract_generator(&proofs_dir)
}

pub fn run_lake_build(proofs_dir: &Path) -> Result<()> {
    let output = Command::new("lake")
        .args(["build", CONTRACT_TARGET])
        .current_dir(proofs_dir)
        .output()
        .with_context(|| {
            format!(
                "failed to build Lean conformance contract target in {}",
                proofs_dir.display()
            )
        })?;

    if !output.status.success() {
        anyhow::bail!(
            "Lean conformance contract build failed\n  cwd: {}\n  status: {}\n  stdout:\n{}\n  stderr:\n{}",
            proofs_dir.display(),
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

pub fn run_contract_generator(proofs_dir: &Path) -> Result<String> {
    let output = Command::new("lake")
        .args(["env", "lean", "--run", CONTRACT_RUN_FILE])
        .current_dir(proofs_dir)
        .output()
        .with_context(|| {
            format!(
                "failed to run Lean conformance contract generator in {}",
                proofs_dir.display()
            )
        })?;

    if !output.status.success() {
        anyhow::bail!(
            "Lean conformance contract generator failed\n  cwd: {}\n  status: {}\n  stdout:\n{}\n  stderr:\n{}",
            proofs_dir.display(),
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    String::from_utf8(output.stdout).context("Lean conformance contract stdout was not UTF-8")
}

pub fn proofs_dir() -> Result<PathBuf> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));

    let direct = manifest_dir.join("proofs");
    if direct.join("lakefile.lean").exists() {
        return Ok(direct);
    }

    for ancestor in manifest_dir.ancestors() {
        let candidate = ancestor.join("crates/defra-agent/proofs");
        if candidate.join("lakefile.lean").exists() {
            return Ok(candidate);
        }
    }

    let sibling = manifest_dir
        .parent()
        .map(|parent| parent.join("defra-agent/proofs"));
    if let Some(candidate) = sibling {
        if candidate.join("lakefile.lean").exists() {
            return Ok(candidate);
        }
    }

    Err(anyhow!(
        "could not locate crates/defra-agent/proofs from {}",
        manifest_dir.display()
    ))
}

pub fn extract_contract_json(stdout: &str) -> Result<&str> {
    let begin = unique_marker_position(stdout, CONTRACT_JSON_BEGIN)?;
    let end = unique_marker_position(stdout, CONTRACT_JSON_END)?;
    anyhow::ensure!(
        begin < end,
        "Lean contract JSON sentinel order is invalid\n  stdout:\n{}",
        stdout
    );

    let json = stdout[begin + CONTRACT_JSON_BEGIN.len()..end].trim();
    anyhow::ensure!(
        !json.is_empty(),
        "Lean contract JSON sentinel block was empty\n  stdout:\n{}",
        stdout
    );
    Ok(json)
}

fn unique_marker_position(stdout: &str, marker: &str) -> Result<usize> {
    let positions = stdout
        .match_indices(marker)
        .map(|(position, _)| position)
        .collect::<Vec<_>>();
    match positions.as_slice() {
        [position] => Ok(*position),
        [] => Err(anyhow!(
            "Lean contract generator stdout did not contain {marker:?}: {stdout}"
        )),
        _ => Err(anyhow!(
            "Lean contract generator stdout contained duplicate {marker:?} sentinels: {stdout}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_contract_json_between_unique_sentinels() {
        let stdout = format!(
            "debug {{noise}}\n{CONTRACT_JSON_BEGIN}\n{{\"ok\":true}}\n{CONTRACT_JSON_END}\nmore {{noise}}\n"
        );

        assert_eq!(extract_contract_json(&stdout).unwrap(), "{\"ok\":true}");
    }

    #[test]
    fn rejects_duplicate_sentinels() {
        let stdout = format!(
            "{CONTRACT_JSON_BEGIN}\n{{\"ok\":true}}\n{CONTRACT_JSON_BEGIN}\n{CONTRACT_JSON_END}\n"
        );

        let err = extract_contract_json(&stdout).unwrap_err().to_string();
        assert!(
            err.contains("duplicate"),
            "expected duplicate sentinel error, got: {err}"
        );
    }
}
