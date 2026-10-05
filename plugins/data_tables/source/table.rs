//! What every data source offers the engine: a schema, a cheap row count when
//! one is known, a fingerprint for cursors, and a streaming scan that can
//! read just the columns a query needs.
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use arrow::datatypes::SchemaRef;
use arrow::record_batch::RecordBatch;

use crate::Res;

/// Rows per batch a scan produces at most.
pub const BATCH_ROWS: usize = 8192;
/// Bytes of source data a batch is built from at most.
pub const BATCH_BYTES: usize = 8 * 1024 * 1024;
/// Rows a type-inference pass reads before it settles, unless asked to read all.
pub const SAMPLE_ROWS: u64 = 100_000;
/// Warnings kept per call; the rest are counted.
const MAX_WARNINGS: usize = 40;

/// How much of a source decides the type of its columns.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Infer {
    /// The first [`SAMPLE_ROWS`] rows.
    Sample,
    /// Every row.
    Full,
}

/// Why a scan stopped.
#[derive(Debug)]
pub enum ScanError {
    /// A value past the sampled rows does not fit its column's inferred type.
    Conflict {
        /// The table's name.
        table: String,
        /// The column's name.
        column: String,
        /// The 1-based line or row of the value.
        line: u64,
    },
    /// Any other failure, as one sentence.
    Failed(String),
}

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict {
                table,
                column,
                line,
            } => write!(
                f,
                "column {column} of {table} holds a value on line {line} that does not fit its type"
            ),
            Self::Failed(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for ScanError {}

impl From<String> for ScanError {
    fn from(s: String) -> Self {
        Self::Failed(s)
    }
}

/// A stream of batches.
pub type Batches = Box<dyn Iterator<Item = Result<RecordBatch, ScanError>> + Send>;

/// One table of data.
pub trait TableSource: Send + Sync {
    /// The table's columns.
    fn schema(&self) -> SchemaRef;
    /// The number of rows, when it is known without a full scan.
    fn row_count(&self) -> Option<u64>;
    /// Reads the table, producing only the columns in `projection` (all when `None`), which
    /// is ascending and has no repeats.
    fn scan(&self, projection: Option<&[usize]>) -> Batches;
    /// A value that changes when the table's content does.
    fn fingerprint(&self) -> u64;
    /// Facts about how the source was read, such as a CSV's delimiter.
    fn details(&self) -> Option<serde_json::Value> {
        None
    }
}

/// What a call noticed about its data, one line per kind.
#[derive(Default)]
pub struct Warnings {
    seen: Mutex<BTreeMap<String, String>>,
}

impl Warnings {
    /// A shared, empty set.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Records `message` under `key` unless `key` already has one.
    pub fn once(&self, key: &str, message: String) {
        if let Ok(mut seen) = self.seen.lock() {
            seen.entry(key.to_string()).or_insert(message);
        }
    }

    /// The warnings in key order, with a closing line when more were dropped.
    pub fn list(&self) -> Vec<String> {
        let Ok(seen) = self.seen.lock() else {
            return Vec::new();
        };
        let mut out: Vec<String> = seen.values().take(MAX_WARNINGS).cloned().collect();
        if seen.len() > MAX_WARNINGS {
            out.push(format!(
                "{} more warnings were left out",
                seen.len() - MAX_WARNINGS
            ));
        }
        out
    }
}

/// FNV-1a, the cheap stable hash behind fingerprints and cursors.
#[derive(Clone, Copy)]
pub struct Fnv(pub u64);

impl Default for Fnv {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Fnv {
    /// Mixes `bytes` in.
    pub fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
    }

    /// Mixes a length-prefixed string in, so adjacent strings cannot blur.
    pub fn text(&mut self, s: &str) {
        self.write(&(s.len() as u64).to_le_bytes());
        self.write(s.as_bytes());
    }

    /// Mixes a number in.
    pub fn num(&mut self, n: u64) {
        self.write(&n.to_le_bytes());
    }
}

/// A fingerprint of a file's content that costs two small reads: its size and
/// its first and last 64 KiB. A change in the middle that keeps the size is not seen.
pub fn file_fingerprint(path: &Path) -> Res<u64> {
    use std::io::{Read, Seek, SeekFrom};
    const EDGE: u64 = 64 * 1024;
    let read_err = |e: std::io::Error| format!("cannot read {}: {e}", path.display());
    let mut f = std::fs::File::open(path).map_err(read_err)?;
    let len = f.metadata().map_err(read_err)?.len();
    let mut h = Fnv::default();
    h.num(len);
    let mut buf = Vec::new();
    f.by_ref()
        .take(EDGE)
        .read_to_end(&mut buf)
        .map_err(read_err)?;
    h.write(&buf);
    if len > EDGE {
        buf.clear();
        f.seek(SeekFrom::Start(len.saturating_sub(EDGE).max(EDGE)))
            .map_err(read_err)?;
        f.read_to_end(&mut buf).map_err(read_err)?;
        h.write(&buf);
    }
    Ok(h.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warnings_keep_the_first_message_per_key_in_key_order() {
        let w = Warnings::new();
        w.once("b", "second".into());
        w.once("a", "first".into());
        w.once("a", "ignored".into());
        assert_eq!(w.list(), ["first", "second"]);
    }

    #[test]
    fn warnings_say_how_many_were_left_out() {
        let w = Warnings::new();
        for i in 0..(MAX_WARNINGS + 3) {
            w.once(&format!("k{i:03}"), format!("m{i}"));
        }
        let list = w.list();
        assert_eq!(list.len(), MAX_WARNINGS + 1);
        assert_eq!(list.last().unwrap(), "3 more warnings were left out");
    }

    #[test]
    fn the_hash_is_stable_and_length_prefixed() {
        let mut a = Fnv::default();
        a.text("ab");
        a.text("c");
        let mut b = Fnv::default();
        b.text("a");
        b.text("bc");
        assert_ne!(a.0, b.0);
        let mut c = Fnv::default();
        c.write(b"hello");
        assert_eq!(c.0, 0xa430_d846_80aa_bd0b);
    }

    #[test]
    fn a_fingerprint_sees_size_and_both_ends_and_nothing_of_the_clock() {
        let dir = std::env::temp_dir().join(format!("dt-fp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("f.bin");
        let mut data = vec![7u8; 300_000];
        std::fs::write(&p, &data).unwrap();
        let base = file_fingerprint(&p).unwrap();
        assert_eq!(base, file_fingerprint(&p).unwrap());
        data[0] = 8;
        std::fs::write(&p, &data).unwrap();
        assert_ne!(base, file_fingerprint(&p).unwrap());
        data[0] = 7;
        data[299_999] = 9;
        std::fs::write(&p, &data).unwrap();
        assert_ne!(base, file_fingerprint(&p).unwrap());
        data.push(7);
        data[299_999] = 7;
        std::fs::write(&p, &data).unwrap();
        assert_ne!(base, file_fingerprint(&p).unwrap());
        assert!(file_fingerprint(&dir.join("missing")).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
