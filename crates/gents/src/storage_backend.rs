use std::path::Path;

use anyhow::Result;

/// Reject a data directory created by the legacy RocksDB runtime backend.
///
/// Lark and RocksDB use incompatible on-disk formats. Lark would otherwise
/// create its own files beside an existing RocksDB store and make the runtime
/// appear empty, so callers must fail before opening the directory.
pub fn reject_legacy_rocksdb_store(data_path: &Path) -> Result<()> {
    let current = data_path.join("CURRENT");
    if current.is_file() {
        anyhow::bail!(
            "{} contains a legacy RocksDB Gents store; this release uses Lark and cannot open it. Reset the runtime state or use an older Gents release to export any data you need first",
            data_path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_rocksdb_current_marker() {
        let tempdir = tempfile::tempdir().unwrap();
        std::fs::write(tempdir.path().join("CURRENT"), "MANIFEST-000005\n").unwrap();

        let error = reject_legacy_rocksdb_store(tempdir.path()).unwrap_err();

        assert!(error.to_string().contains("legacy RocksDB Gents store"));
    }

    #[test]
    fn accepts_empty_or_lark_data_directory() {
        let tempdir = tempfile::tempdir().unwrap();
        reject_legacy_rocksdb_store(tempdir.path()).unwrap();

        std::fs::write(tempdir.path().join("MANIFEST"), "lark").unwrap();
        reject_legacy_rocksdb_store(tempdir.path()).unwrap();
    }
}
