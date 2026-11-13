//! Retention and program sharing for `service-updates/repair-*` backups
//! (`Proofs/Clients/RepairBackups.lean`).
use super::files;
use anyhow::Result;
use std::path::{Path, PathBuf};

/// `RepairBackups.limit`.
pub(super) const KEPT: usize = 3;
const PROGRAM: &str = "prior-gents";
const PROGRAM_DIGEST: &str = "prior-gents.sha256";

/// `RepairBackups.retained` over backups newest first.
pub(super) fn retained<'a, I>(newest_first: &'a [I], in_use: Option<&I>) -> Vec<&'a I>
where
    I: PartialEq,
{
    let (newest, older) = newest_first.split_at(newest_first.len().min(KEPT));
    newest
        .iter()
        .chain(older.iter().find(|backup| Some(*backup) == in_use))
        .collect()
}

pub(super) struct Recorded<I, D> {
    pub(super) id: I,
    pub(super) program: Option<D>,
}

/// `RepairBackups.sharedWith`.
pub(super) fn shared_with<'a, I, D>(
    kept: &'a [Recorded<I, D>],
    program: Option<&D>,
) -> Option<&'a I>
where
    D: PartialEq,
{
    let program = program?;
    kept.iter()
        .find(|backup| backup.program.as_ref() == Some(program))
        .map(|backup| &backup.id)
}

/// The other `repair-*` directories under `root`, newest first. Their names
/// are random, so creation time orders them where the filesystem records it.
fn older_backups(root: &Path, current: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path == current
            || !entry.file_type()?.is_dir()
            || !entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("repair-"))
        {
            continue;
        }
        let metadata = entry.metadata()?;
        found.push((metadata.created().or_else(|_| metadata.modified())?, path));
    }
    found.sort_by(|a, b| b.cmp(a));
    Ok(found.into_iter().map(|(_, path)| path).collect())
}

/// Saves the service program `original` as `directory/prior-gents` and records
/// its digest. A kept backup's program of that digest is hard-linked rather
/// than copied; the link is checked against the digest because the kept file's
/// record is only as good as that file.
pub(super) fn save_program(root: &Path, directory: &Path, original: &Path) -> Result<PathBuf> {
    let digest = files::digest(original)?;
    let kept: Vec<_> = older_backups(root, directory)?
        .into_iter()
        .map(|id| Recorded {
            program: files::read_json::<String>(&id.join(PROGRAM_DIGEST)).ok(),
            id,
        })
        .collect();
    let saved = directory.join(PROGRAM);
    let mut linked = shared_with(&kept, Some(&digest))
        .is_some_and(|source| std::fs::hard_link(source.join(PROGRAM), &saved).is_ok());
    if linked && files::digest(&saved)? != digest {
        std::fs::remove_file(&saved)?;
        linked = false;
    }
    if !linked {
        files::copy_executable(original, &saved)?;
    }
    files::write_json(&directory.join(PROGRAM_DIGEST), &digest)?;
    Ok(saved)
}

/// Removes the repair backups `retained` drops. `current` is this repair's
/// backup; `program` is what the installed service definition runs, which may
/// be a restored backup's `prior-gents`.
pub(super) fn prune(root: &Path, current: &Path, program: &Path) -> Result<()> {
    let in_use = std::fs::canonicalize(program)
        .ok()
        .and_then(|program| program.parent().map(Path::to_path_buf));
    let mut newest_first = vec![current.to_path_buf()];
    newest_first.extend(older_backups(root, current)?);
    let canonical = newest_first
        .iter()
        .map(std::fs::canonicalize)
        .collect::<std::io::Result<Vec<_>>>()?;
    let kept = retained(&canonical, in_use.as_ref());
    for (path, canonical) in newest_first.iter().zip(&canonical) {
        if !kept.contains(&canonical) {
            std::fs::remove_dir_all(path)?;
        }
    }
    Ok(())
}
