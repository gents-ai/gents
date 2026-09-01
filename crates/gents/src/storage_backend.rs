use std::io::Read;
use std::path::Path;

use anyhow::Result;

/// Reject a data directory created by a retired runtime storage backend.
///
/// Regolith, Lark, and RocksDB use incompatible on-disk formats. Regolith
/// would otherwise try to open legacy files as its own store, so callers must
/// fail before opening the directory.
pub fn reject_legacy_store(data_path: &Path) -> Result<()> {
    let current = data_path.join("CURRENT");
    if current.is_file() {
        anyhow::bail!(
            "{} contains a legacy RocksDB Gents store; this release uses Regolith and cannot open it. Reset the runtime state or use an older Gents release to export any data you need first",
            data_path.display()
        );
    }

    let lark_path = data_path.join("data.lark");
    if lark_path.exists() {
        anyhow::bail!(
            "{} contains a legacy Lark Gents store; this release uses Regolith and cannot open it. Reset the runtime state or use an older Gents release to export any data you need first",
            data_path.display()
        );
    }

    let manifest_path = data_path.join("MANIFEST");
    if manifest_path.is_file() {
        let mut manifest = std::fs::File::open(&manifest_path)?;
        let mut magic = [0_u8; 7];
        let read = manifest.read(&mut magic)?;
        if read != magic.len() || magic != *b"REGOMAN" {
            anyhow::bail!(
                "{} contains an unsupported or corrupt Gents store; this release uses Regolith and cannot open it. Reset the runtime state or use an older Gents release to export any data you need first",
                data_path.display()
            );
        }
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

        let error = reject_legacy_store(tempdir.path()).unwrap_err();

        assert!(error.to_string().contains("legacy RocksDB Gents store"));
    }

    #[test]
    fn rejects_lark_manifest() {
        let tempdir = tempfile::tempdir().unwrap();
        std::fs::create_dir(tempdir.path().join("data.lark")).unwrap();

        let error = reject_legacy_store(tempdir.path()).unwrap_err();

        assert!(error.to_string().contains("legacy Lark Gents store"));
    }

    #[test]
    fn rejects_unknown_manifest_format() {
        let tempdir = tempfile::tempdir().unwrap();
        std::fs::write(tempdir.path().join("MANIFEST"), "unknown").unwrap();

        let error = reject_legacy_store(tempdir.path()).unwrap_err();

        assert!(error.to_string().contains("unsupported or corrupt"));
    }

    #[test]
    fn accepts_empty_or_regolith_data_directory() {
        let tempdir = tempfile::tempdir().unwrap();
        reject_legacy_store(tempdir.path()).unwrap();

        std::fs::write(tempdir.path().join("MANIFEST"), b"REGOMAN\x01rest").unwrap();
        reject_legacy_store(tempdir.path()).unwrap();
    }
}
