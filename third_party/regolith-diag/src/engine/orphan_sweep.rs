//! Remove SSTables the manifest does not reference.
//!
//! Every path that deletes a table does so after the manifest stops naming
//! it, and best effort: a crash, a failed unlink, or a compaction that
//! failed after offering its outputs to the manifest can each leave a file
//! that nothing will ever read or delete. Left alone they accumulate
//! without bound, so a writable open removes them.
//!
//! Only ids below the replayed `next_file_id` are candidates. Those were
//! allocated by this database and are not named by the version it just
//! replayed, so nothing can reach them. A file at or above `next_file_id`
//! may belong to an allocation whose manifest record never became durable;
//! it is left in place, and the open moves `next_file_id` past it
//! ([`next_id_past_tables`]), so its id is never handed out again and no
//! later table or log is written over it.
//!
//! The caller holds the exclusive directory lock and only sweeps where
//! that lock excludes other processes ([`crate::env::Capabilities::file_lock`]),
//! so no other writer can be creating one of these files. Removing them
//! is idempotent: a crash part way through leaves a directory the next
//! open sweeps the same way.
//!
//! The directory is walked one entry at a time and each orphan is removed
//! as the walk reaches it, so the sweep holds the version's live ids,
//! which the open holds anyway, and one entry, never the directory.

use std::collections::HashSet;
use std::io;
use std::path::Path;

use super::manifest::Version;
use crate::env::Env;

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct SweepReport {
    pub(crate) files: usize,
    pub(crate) bytes: u64,
    /// The highest id of a table the sweep left in the directory.
    pub(crate) highest_kept: Option<u64>,
}

/// The `next_file_id` an open hands ids out from: past every table in the
/// directory as well as past what the manifest recorded. A file the
/// manifest does not name, left by a crash or by a manifest that lost its
/// tail, is never written over by a table or a log that takes its id.
pub(crate) fn next_id_past_tables(next_file_id: u64, highest_table: Option<u64>) -> u64 {
    highest_table.map_or(next_file_id, |id| next_file_id.max(id.saturating_add(1)))
}

/// The highest id among the tables in `sst_dir`, for an open that does not
/// sweep it. One walk, one entry at a time; a directory that does not exist
/// holds none.
pub(crate) fn highest_table_id(env: &dyn Env, sst_dir: &Path) -> io::Result<Option<u64>> {
    if !env.exists(sst_dir) {
        return Ok(None);
    }
    let mut highest = None;
    for entry in env.read_dir(sst_dir)? {
        if let Some(id) = table_id(&entry?.path) {
            highest = highest.max(Some(id));
        }
    }
    Ok(highest)
}

pub(crate) fn sweep_unreferenced_tables(
    env: &dyn Env,
    sst_dir: &Path,
    version: &Version,
) -> io::Result<SweepReport> {
    let live: HashSet<u64> = version
        .levels
        .iter()
        .flat_map(|level| level.iter().map(|file| file.meta.file_id))
        .collect();

    let mut report = SweepReport::default();
    if !env.exists(sst_dir) {
        return Ok(report);
    }
    for entry in env.read_dir(sst_dir)? {
        let entry = entry?;
        // A table renamed aside on removal while a handle read it (see
        // `env::open_file_limit`): its handles died with the process.
        let removed = crate::env::is_removed_table(&entry.path);
        if !removed {
            let Some(id) = table_id(&entry.path) else {
                continue;
            };
            if live.contains(&id) || id >= version.next_file_id {
                report.highest_kept = report.highest_kept.max(Some(id));
                continue;
            }
        }
        let len = env.metadata(&entry.path).map(|m| m.len).unwrap_or(0);
        match env.remove_file(&entry.path) {
            Ok(()) => {
                report.files += 1;
                report.bytes += len;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(
                path = %entry.path.display(),
                error = %e,
                "could not remove an unreferenced SSTable"
            ),
        }
    }

    if report.files > 0 {
        env.sync_dir(sst_dir)?;
        tracing::warn!(
            files = report.files,
            bytes = report.bytes,
            dir = %sst_dir.display(),
            "removed SSTables the manifest does not reference"
        );
    }
    Ok(report)
}

fn table_id(path: &Path) -> Option<u64> {
    if path.extension()? != "sst" {
        return None;
    }
    path.file_stem()?.to_str()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::std_env;

    fn version_with_next_id(next_file_id: u64) -> Version {
        let mut version = Version::new();
        version.next_file_id = next_file_id;
        version
    }

    fn touch(dir: &Path, name: &str, len: usize) {
        std::fs::write(dir.join(name), vec![0u8; len]).unwrap();
    }

    #[test]
    fn removes_unreferenced_tables_below_the_next_file_id() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "000003.sst", 10);
        touch(dir.path(), "000007.sst", 20);
        let report =
            sweep_unreferenced_tables(&*std_env(), dir.path(), &version_with_next_id(10)).unwrap();
        assert_eq!(
            report,
            SweepReport {
                files: 2,
                bytes: 30,
                highest_kept: None,
            }
        );
        assert!(!dir.path().join("000003.sst").exists());
        assert!(!dir.path().join("000007.sst").exists());
    }

    #[test]
    fn keeps_ids_at_or_above_the_next_file_id() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "000010.sst", 1);
        touch(dir.path(), "000042.sst", 1);
        let report =
            sweep_unreferenced_tables(&*std_env(), dir.path(), &version_with_next_id(10)).unwrap();
        assert_eq!(report.files, 0);
        assert!(dir.path().join("000010.sst").exists());
        assert!(dir.path().join("000042.sst").exists());
    }

    #[test]
    fn removes_tables_renamed_aside_on_removal() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "000003.sst.removed-17", 10);
        touch(dir.path(), "000004.sst", 1);
        let report =
            sweep_unreferenced_tables(&*std_env(), dir.path(), &version_with_next_id(4)).unwrap();
        assert_eq!(report.files, 1);
        assert!(!dir.path().join("000003.sst.removed-17").exists());
        assert!(dir.path().join("000004.sst").exists());
    }

    #[test]
    fn ignores_files_that_are_not_tables() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "000001.tmp", 1);
        touch(dir.path(), "notes.sst", 1);
        touch(dir.path(), "LOCK", 0);
        let report =
            sweep_unreferenced_tables(&*std_env(), dir.path(), &version_with_next_id(10)).unwrap();
        assert_eq!(report.files, 0);
        for name in ["000001.tmp", "notes.sst", "LOCK"] {
            assert!(dir.path().join(name).exists(), "{name} removed");
        }
    }

    /// The sweep reports the highest table it kept, and the open's next id
    /// moves past it; with none kept, the manifest's next id stands.
    #[test]
    fn the_next_id_moves_past_every_table_left_in_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "000003.sst", 0);
        touch(dir.path(), "000042.sst", 1);
        touch(dir.path(), "000007.tmp", 1);
        let report =
            sweep_unreferenced_tables(&*std_env(), dir.path(), &version_with_next_id(10)).unwrap();
        assert_eq!(report.highest_kept, Some(42));
        assert_eq!(next_id_past_tables(10, report.highest_kept), 43);
        assert_eq!(next_id_past_tables(10, None), 10);
        assert_eq!(next_id_past_tables(50, Some(42)), 50);

        assert_eq!(highest_table_id(&*std_env(), dir.path()).unwrap(), Some(42));
        assert_eq!(
            highest_table_id(&*std_env(), &dir.path().join("none")).unwrap(),
            None
        );
    }

    #[test]
    fn a_missing_directory_sweeps_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let report = sweep_unreferenced_tables(
            &*std_env(),
            &dir.path().join("sst"),
            &version_with_next_id(10),
        )
        .unwrap();
        assert_eq!(report, SweepReport::default());
    }
}
