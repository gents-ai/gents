//! Retiring the write-ahead logs whose writes are in tables (E30).
//!
//! A flush makes its memtable's log redundant with the one synced manifest
//! batch that adds the table: the same batch records, as `min_wal_id`,
//! that every log up to the flushed one holds only writes a table holds.
//! Recovery replays only logs at or above `min_wal_id`
//! (`should_replay_wal`), so a redundant log is never replayed, whether or
//! not its removal succeeded. Replaying one would put its versions in the
//! memtable, which a read consults before any table, so a key rewritten
//! since, by a later flush or an ingest, would read back as its older value.
//!
//! Removal is then a matter of disk space. A failed one is reported, a warn
//! line and [`Ticker::WalRemoveFailed`], and retried: by the next flush,
//! which sweeps every log below the one it retires, and by the next open,
//! which removes every log below the one it starts. Logs are retired oldest
//! first, so a log below a retired one is redundant too.

use std::path::{Path, PathBuf};

use crate::env::Env;
use crate::portability::{AtomicBool, Ordering};
use crate::statistics::{Statistics, Ticker};

use super::memtable::MemTable;
use super::wal::Wal;

/// The logs a flush leaves behind, and whether a removal is owed.
pub(crate) struct RetiredLogs {
    wal_dir: PathBuf,
    /// Set when a removal failed; the next retirement sweeps the directory.
    owed: AtomicBool,
}

impl RetiredLogs {
    /// Retirement for the logs in `wal_dir`, owing a sweep when `owed`.
    pub(crate) fn new(wal_dir: PathBuf, owed: bool) -> Self {
        Self {
            wal_dir,
            owed: AtomicBool::new(owed),
        }
    }

    /// Remove the log `flushed` was sealed with, once a durable manifest
    /// batch holds its writes in a table, and every log below it a failed
    /// removal left behind.
    ///
    /// Caller holds the flush exclusion, so retirements run in seal order.
    pub(crate) fn retire(&self, env: &dyn Env, flushed: &MemTable, stats: Option<&Statistics>) {
        let Some(path) = flushed.sealed_wal() else {
            return;
        };
        if !remove_log(env, path, stats) {
            self.owed.store(true, Ordering::Release);
            return;
        }
        if self.owed.swap(false, Ordering::AcqRel)
            && let Some(id) = super::wal_file_id(path)
            && !remove_below(env, &self.wal_dir, id, stats)
        {
            self.owed.store(true, Ordering::Release);
        }
    }
}

/// The `min_wal_id` a flush of `flushed` records with its table: every log
/// up to the one `flushed` was sealed with is then in tables.
pub(crate) fn min_wal_id_after(flushed: &MemTable) -> Option<u64> {
    super::wal_file_id(flushed.sealed_wal()?).map(|id| id + 1)
}

/// Remove every log in `wal_dir` whose id is below `min_wal_id`, reporting
/// each one that stays. Returns whether none stayed.
pub(crate) fn remove_below(
    env: &dyn Env,
    wal_dir: &Path,
    min_wal_id: u64,
    stats: Option<&Statistics>,
) -> bool {
    let unlisted = |error: std::io::Error| {
        tracing::warn!(
            dir = %wal_dir.display(),
            %error,
            "could not list the write-ahead logs to remove the ones in tables; trying again at the next flush or open"
        );
        false
    };
    let logs = match super::wal_scan::walk(env, wal_dir) {
        Ok(logs) => logs,
        Err(error) => return unlisted(error),
    };
    // Each log is removed as the walk reaches it, and every one is tried,
    // whether or not one before it stayed. A walk cut short by an error
    // leaves the rest for the retry the `false` asks for.
    let mut none_stayed = true;
    for log in logs {
        let path = match log {
            Ok(path) => path,
            Err(error) => return unlisted(error),
        };
        if super::wal_file_id(&path).is_some_and(|id| id < min_wal_id) {
            none_stayed &= remove_log(env, &path, stats);
        }
    }
    none_stayed
}

/// Remove one log whose writes are in tables. A log already gone counts as
/// removed; any other failure is logged and counted, and returns `false`.
fn remove_log(env: &dyn Env, path: &Path, stats: Option<&Statistics>) -> bool {
    match Wal::remove_in(env, path) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                %error,
                "could not remove a write-ahead log whose writes are in tables; it is never replayed, and the next flush or open removes it"
            );
            if let Some(stats) = stats {
                stats.add(Ticker::WalRemoveFailed, 1);
            }
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::MemEnv;
    use crate::env::walk_fault::WalkFault;

    fn logs(env: &dyn Env, ids: impl IntoIterator<Item = u64>) {
        env.create_dir_all(Path::new("/db/wal")).unwrap();
        for id in ids {
            let path = PathBuf::from(format!("/db/wal/{}", crate::engine::wal::wal_filename(id)));
            env.write(&path, b"").unwrap();
        }
    }

    fn names(env: &dyn Env) -> Vec<String> {
        let mut names: Vec<String> = env
            .read_dir(Path::new("/db/wal"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        names
    }

    /// Thousands of retired logs, far more than one bucket of the map the
    /// walk goes through, are removed as the walk reaches them; the live
    /// ones and the files that are not logs stay.
    #[test]
    fn removes_every_log_below_the_minimum_and_nothing_else() {
        let env = MemEnv::new();
        logs(&env, 0..5_003);
        env.write(Path::new("/db/wal/notes"), b"").unwrap();
        assert!(remove_below(&env, Path::new("/db/wal"), 5_000, None));
        assert_eq!(
            names(&env),
            vec![
                "notes",
                "wal_005000.log",
                "wal_005001.log",
                "wal_005002.log"
            ]
        );
    }

    /// A walk that breaks part way removes what it reached and owes a
    /// retry; the retry, over a whole walk, removes the rest.
    #[test]
    fn a_walk_that_breaks_owes_a_retry() {
        let env = WalkFault::default();
        logs(&env, 0..100);
        env.arm(Path::new("/db/wal"), 10);
        assert!(!remove_below(&env, Path::new("/db/wal"), 100, None));
        assert_eq!(names(&env.inner).len(), 90);
        env.arm(Path::new("/nowhere"), 0);
        assert!(remove_below(&env, Path::new("/db/wal"), 100, None));
        assert!(names(&env).is_empty());
    }
}
