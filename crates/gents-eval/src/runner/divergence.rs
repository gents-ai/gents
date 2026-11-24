use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use gents_documents::EvalDefinition;
use gents_pack::pack::core_packs::CorePack;

use super::files::write_json_atomically;

pub const DIVERGENCES_FILE: &str = "divergences.json";

/// `Narrows` removes, narrows or replaces what the shipped Engineer has or
/// runs under; `Adds` is a fixture a user's node could plausibly hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Narrows,
    Adds,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Divergence {
    pub severity: Severity,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cell: Option<String>,
    pub detail: String,
}

impl Divergence {
    pub fn narrows(kind: &str, detail: impl Into<String>) -> Self {
        Self {
            severity: Severity::Narrows,
            kind: kind.to_owned(),
            cell: None,
            detail: detail.into(),
        }
    }

    pub fn adds(kind: &str, detail: impl Into<String>) -> Self {
        Self {
            severity: Severity::Adds,
            kind: kind.to_owned(),
            cell: None,
            detail: detail.into(),
        }
    }

    pub fn in_cell(self, cell: &str) -> Self {
        Self {
            cell: Some(cell.to_owned()),
            ..self
        }
    }
}

pub fn definition_divergences(definition: &EvalDefinition) -> Vec<Divergence> {
    let init: Vec<&str> = CorePack::ALL.iter().map(|pack| pack.coordinate()).collect();
    let mut found = Vec::new();
    if definition
        .subject
        .core_packs
        .iter()
        .map(String::as_str)
        .ne(init.iter().copied())
    {
        found.push(Divergence::narrows(
            "core_packs",
            format!(
                "each trial installs the core packs [{}], not the [{}] init installs",
                definition.subject.core_packs.join(", "),
                init.join(", ")
            ),
        ));
    }
    for pack in &definition.subject.enabled_core_packs {
        found.push(Divergence::adds(
            "enabled_core_pack",
            format!(
                "{pack} is turned on as `gents config pack starter --enable {pack}` turns it on; init leaves it off"
            ),
        ));
    }
    found
}

pub fn build_divergence(source_commit: &str, source_dirty: bool) -> Option<Divergence> {
    source_dirty.then(|| {
        Divergence::narrows(
            "dirty_build",
            format!("gents was built from {source_commit} with uncommitted changes"),
        )
    })
}

pub fn read_divergences(run_dir: &Path) -> Option<Vec<Divergence>> {
    serde_json::from_slice(&std::fs::read(run_dir.join(DIVERGENCES_FILE)).ok()?).ok()
}

pub(crate) fn write_divergences(run_dir: &Path, divergences: &[Divergence]) -> Result<()> {
    write_json_atomically(&run_dir.join(DIVERGENCES_FILE), &divergences)
}
