use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::de::DeserializeOwned;

pub const CONTRACT_JSON_BEGIN: &str = "---BEGIN DEFRA LEAN CONTRACT JSON---";
pub const CONTRACT_JSON_END: &str = "---END DEFRA LEAN CONTRACT JSON---";

// Build only the module artifact needed by `lake env lean --run`; the broader
// target can pull package extra artifacts such as ProofWidgets' widget bundle.
const CONTRACT_TARGET: &str = "Proofs.Conformance.Contracts:olean";
const CONTRACT_RUN_FILE: &str = "Proofs/Conformance/Contracts.lean";
const LAKE_BUILD_ATTEMPTS: usize = 3;

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
    let mut failures = Vec::new();

    for attempt in 1..=LAKE_BUILD_ATTEMPTS {
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

        if output.status.success() {
            return Ok(());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let retryable = is_retryable_lake_build_failure(&stdout, &stderr);
        failures.push(format!(
            "attempt {attempt}/{LAKE_BUILD_ATTEMPTS}\n  status: {}\n  stdout:\n{}\n  stderr:\n{}",
            output.status, stdout, stderr
        ));

        if !retryable || attempt == LAKE_BUILD_ATTEMPTS {
            anyhow::bail!(
                "Lean conformance contract build failed\n  cwd: {}\n{}",
                proofs_dir.display(),
                failures.join("\n\n")
            );
        }

        std::thread::sleep(Duration::from_secs((attempt as u64) * 5));
    }

    unreachable!("lake build retry loop should return or bail")
}

fn is_retryable_lake_build_failure(stdout: &str, stderr: &str) -> bool {
    let combined = format!("{stdout}\n{stderr}");
    combined.contains("external command 'git' exited with code 128")
        || combined.contains("failed to fetch GitHub release")
        || combined.contains("ProofWidgets not up-to-date")
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

    #[test]
    fn retries_transient_lake_dependency_fetch_failures() {
        assert!(is_retryable_lake_build_failure(
            "",
            "info: mathlib: cloning https://github.com/leanprover-community/mathlib4\n\
             error: external command 'git' exited with code 128"
        ));
    }

    #[test]
    fn does_not_retry_ordinary_lean_build_errors() {
        assert!(!is_retryable_lake_build_failure(
            "",
            "error: Proofs/Foo.lean:12:4: unknown identifier 'bar'"
        ));
    }
}
