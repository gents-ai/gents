//! The order OPFS mirror mode writes a batch of dirty files back in (E20).
//!
//! Mirror mode holds the database in memory and `OpfsEnv::persist` writes
//! every dirty file out whole, one after another, with nothing ordering one
//! file's arrival in storage against another's, and a tab can close part
//! way through a batch. So the batch is written tables first, then every
//! other file (the logs), then the MANIFEST that names tables, and its
//! deletions last, after the manifest that stopped naming what they remove.
//! A batch cut short at any point then leaves the persisted manifest naming
//! only tables that are persisted. regolith keeps no `CURRENT` file: the
//! MANIFEST is replaced whole, by rename, so it is its own pointer.

use std::path::Path;

/// Where `path` goes in a persist batch, lower first: 0 for a table, 2 for
/// the MANIFEST, 1 for everything else.
pub(crate) fn persist_rank(path: &Path) -> u8 {
    if path.file_name().is_some_and(|name| name == "MANIFEST") {
        2
    } else if path.extension().is_some_and(|ext| ext == "sst") {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn tables_go_first_and_the_manifest_last() {
        assert_eq!(persist_rank(Path::new("db/sst/000007.sst")), 0);
        assert_eq!(persist_rank(Path::new("db/wal/wal_000003.log")), 1);
        assert_eq!(persist_rank(Path::new("db/MANIFEST.tmp")), 1);
        assert_eq!(persist_rank(Path::new("db/LOCK")), 1);
        assert_eq!(persist_rank(Path::new("db/MANIFEST")), 2);
    }

    #[test]
    fn a_sorted_batch_never_puts_the_manifest_before_a_table() {
        let mut batch: Vec<PathBuf> = [
            "db/MANIFEST",
            "db/wal/wal_000009.log",
            "db/sst/000010.sst",
            "db/MANIFEST.tmp",
            "db/sst/000004.sst",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        batch.sort_by_key(|path| persist_rank(path));
        let manifest = batch
            .iter()
            .position(|path| path.ends_with("MANIFEST"))
            .unwrap();
        assert_eq!(manifest, batch.len() - 1);
        assert!(
            batch[..2]
                .iter()
                .all(|path| path.extension().is_some_and(|ext| ext == "sst"))
        );
    }
}
