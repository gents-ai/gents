use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::documents::definition_ref;
use super::{refused, RunSidecar};
use crate::document_config::EvalDefinition;
use crate::eval::runner::executor::Capture;
use crate::eval::DefinitionRef;

pub(super) fn sidecar_path(run_dir: &Path) -> PathBuf {
    run_dir.join("run.json")
}

pub(super) fn read_sidecar(run_dir: &Path) -> Result<Option<RunSidecar>> {
    let path = sidecar_path(run_dir);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .with_context(|| format!("parsing {}", path.display()))
}

pub(crate) fn frozen_captures(run_dir: &Path) -> Result<Option<Vec<Capture>>> {
    Ok(read_sidecar(run_dir)?.map(|sidecar| sidecar.captures))
}

pub(super) fn write_sidecar(run_dir: &Path, sidecar: &RunSidecar) -> Result<()> {
    std::fs::create_dir_all(run_dir).with_context(|| format!("creating {}", run_dir.display()))?;
    let path = sidecar_path(run_dir);
    crate::eval::runner::files::write_json_atomically(&path, sidecar)
        .with_context(|| format!("writing {}", path.display()))
}

pub const DEFINITION_FILE: &str = "definition.json";
pub(super) fn write_frozen_definition(run_dir: &Path, definition: &EvalDefinition) -> Result<()> {
    std::fs::create_dir_all(run_dir).with_context(|| format!("creating {}", run_dir.display()))?;
    let path = run_dir.join(DEFINITION_FILE);
    crate::eval::runner::files::write_json_atomically(&path, definition)
        .with_context(|| format!("writing {}", path.display()))
}

pub fn read_frozen_definition(
    run_dir: &Path,
    frozen: &DefinitionRef,
) -> Result<Option<EvalDefinition>> {
    let path = run_dir.join(DEFINITION_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let definition: EvalDefinition =
        serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))?;
    if definition_ref(&definition)?.digest != frozen.digest {
        return Err(refused(format!(
            "{} no longer digests to the definition its run froze ({})",
            path.display(),
            frozen.digest
        )));
    }
    Ok(Some(definition))
}
