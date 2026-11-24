//! Byte-level mutators and on-disk file locators.
//!
//! These edit a closed database directory in place. Nothing here models a
//! crash: they express deliberate corruption, the "an adversary or a bad
//! sector changed these bytes" family. Power loss lives in
//! [`super::power`].
//!
//! None of them syncs: the database reopens in this process and reads the
//! edit back through the page cache, so a sync makes no test stronger, and
//! on macOS it is a full device-cache flush per mutation.

use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Cut `path` down to `offset` bytes.
///
/// Panics if `path` cannot be opened, so a mis-typed path fails the test
/// instead of silently corrupting nothing.
pub fn truncate_at(path: &Path, offset: u64) {
    let f = OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap_or_else(|e| panic!("truncate_at: open {}: {e}", path.display()));
    f.set_len(offset)
        .unwrap_or_else(|e| panic!("truncate_at: set_len {} -> {offset}: {e}", path.display()));
}

/// Flip bit `bit` (0 = least significant) of the byte at `byte_offset`.
///
/// Panics when the offset is past the end of the file: a flip that lands
/// outside the file would be a silent no-op and would make a corruption
/// test pass for the wrong reason.
pub fn flip_bit(path: &Path, byte_offset: u64, bit: u8) {
    assert!(bit < 8, "flip_bit: bit {bit} is not in 0..8");
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap_or_else(|e| panic!("flip_bit: open {}: {e}", path.display()));
    let len = f.metadata().expect("flip_bit: metadata").len();
    assert!(
        byte_offset < len,
        "flip_bit: offset {byte_offset} is past the end of {} (len {len})",
        path.display(),
    );
    f.seek(SeekFrom::Start(byte_offset))
        .expect("flip_bit: seek");
    let mut b = [0u8; 1];
    f.read_exact(&mut b).expect("flip_bit: read");
    b[0] ^= 1 << bit;
    f.seek(SeekFrom::Start(byte_offset))
        .expect("flip_bit: seek");
    f.write_all(&b).expect("flip_bit: write");
}

/// Overwrite `bytes.len()` bytes at `offset`, extending the file if the
/// range runs past the current end.
pub fn overwrite_range(path: &Path, offset: u64, bytes: &[u8]) {
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap_or_else(|e| panic!("overwrite_range: open {}: {e}", path.display()));
    f.seek(SeekFrom::Start(offset))
        .expect("overwrite_range: seek");
    f.write_all(bytes).expect("overwrite_range: write");
}

/// Deterministic filler bytes for an overwrite: a seeded xorshift stream,
/// so a corruption test that fails can be replayed exactly.
pub fn garbage(seed: u64, len: usize) -> Vec<u8> {
    let mut s = seed | 1;
    (0..len)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 24) as u8
        })
        .collect()
}

/// Byte length of `path`, or 0 when it does not exist.
pub fn file_len(path: &Path) -> u64 {
    fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn sorted_with_ext(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = match fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some(ext))
            .collect(),
        Err(_) => Vec::new(),
    };
    out.sort();
    out
}

/// Every `<db>/wal/wal_NNNNNN.log`, ascending by name, so index 0 is the
/// oldest live WAL and the last entry is the one being appended to.
pub fn find_wals(db_dir: &Path) -> Vec<PathBuf> {
    sorted_with_ext(&db_dir.join("wal"), "log")
}

/// The WAL currently being appended to. Panics when the database has
/// none, which would mean a corruption test was about to silently target
/// nothing.
pub fn newest_wal(db_dir: &Path) -> PathBuf {
    find_wals(db_dir)
        .pop()
        .unwrap_or_else(|| panic!("no WAL under {}", db_dir.join("wal").display()))
}

/// `<db>/MANIFEST`, the append-only `VersionEdit` log.
pub fn find_manifest(db_dir: &Path) -> PathBuf {
    db_dir.join("MANIFEST")
}

/// Every `<db>/sst/NNNNNN.sst`, ascending by file id.
pub fn find_ssts(db_dir: &Path) -> Vec<PathBuf> {
    sorted_with_ext(&db_dir.join("sst"), "sst")
}

/// The lowest-numbered SSTable. Panics when the database has none.
pub fn first_sst(db_dir: &Path) -> PathBuf {
    let mut all = find_ssts(db_dir);
    if all.is_empty() {
        panic!("no SSTable under {}", db_dir.join("sst").display());
    }
    all.remove(0)
}

/// Recursively copy a database directory, so a test can mutate a copy and
/// keep the pristine original for comparison.
pub fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("copy_tree: create_dir_all");
    for entry in fs::read_dir(from).expect("copy_tree: read_dir") {
        let entry = entry.expect("copy_tree: dir entry");
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if entry.file_type().expect("copy_tree: file_type").is_dir() {
            copy_tree(&src, &dst);
        } else {
            fs::copy(&src, &dst).expect("copy_tree: copy");
        }
    }
}
