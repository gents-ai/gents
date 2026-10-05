//! Arrow IPC files (`.arrow`, `.feather`) and streams, read batch by batch.
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use arrow::datatypes::SchemaRef;
use arrow::ipc::reader::{FileReader, StreamReader};

use crate::Res;
use crate::table::{Batches, ScanError, TableSource, file_fingerprint};

/// The magic bytes that open an Arrow IPC file.
pub const MAGIC: &[u8] = b"ARROW1";

/// An Arrow IPC file or stream as a table.
pub struct IpcTable {
    path: PathBuf,
    schema: SchemaRef,
    file_format: bool,
    fp: u64,
}

fn unreadable(path: &Path, why: &dyn std::fmt::Display) -> String {
    format!(
        "{} is not a readable Arrow file ({why}); check it was fully written",
        path.display()
    )
}

fn is_file_format(path: &Path) -> Res<bool> {
    use std::io::Read;
    let mut head = [0u8; 6];
    let n = File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(head[..n] == *MAGIC)
}

impl IpcTable {
    /// Reads the schema of `path`.
    pub fn open(path: &Path) -> Res<Self> {
        let file_format = is_file_format(path)?;
        let open = || File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()));
        let schema = if file_format {
            FileReader::try_new(BufReader::new(open()?), None).map(|r| r.schema())
        } else {
            StreamReader::try_new(BufReader::new(open()?), None).map(|r| r.schema())
        }
        .map_err(|e| unreadable(path, &e))?;
        Ok(Self {
            path: path.to_path_buf(),
            schema,
            file_format,
            fp: file_fingerprint(path)?,
        })
    }
}

impl TableSource for IpcTable {
    fn schema(&self) -> SchemaRef {
        SchemaRef::clone(&self.schema)
    }

    fn row_count(&self) -> Option<u64> {
        None
    }

    fn fingerprint(&self) -> u64 {
        self.fp
    }

    fn scan(&self, projection: Option<&[usize]>) -> Batches {
        let fail = |e: String| -> Batches { Box::new(std::iter::once(Err(ScanError::Failed(e)))) };
        let file = match File::open(&self.path) {
            Ok(f) => BufReader::new(f),
            Err(e) => return fail(format!("cannot read {}: {e}", self.path.display())),
        };
        let projection = projection.map(<[usize]>::to_vec);
        let path = self.path.clone();
        let map = move |r: Result<arrow::record_batch::RecordBatch, arrow::error::ArrowError>| {
            r.map_err(|e| ScanError::Failed(unreadable(&path, &e)))
        };
        if self.file_format {
            match FileReader::try_new(file, projection) {
                Ok(r) => Box::new(r.map(map)),
                Err(e) => fail(unreadable(&self.path, &e)),
            }
        } else {
            match StreamReader::try_new(file, projection) {
                Ok(r) => Box::new(r.map(map)),
                Err(e) => fail(unreadable(&self.path, &e)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{Dir, collect, columns, fixtures, sample_columns, sample_rows};
    use arrow::ipc::CompressionType;

    fn open(bytes: &[u8]) -> Result<(IpcTable, Dir), String> {
        let dir = Dir::new();
        let p = dir.put("t.arrow", bytes);
        IpcTable::open(&p).map(|t| (t, dir))
    }

    #[test]
    fn files_streams_and_compressed_files_read_back_the_same_rows() {
        let batch = fixtures::sample_batch();
        for (name, bytes) in [
            ("file", fixtures::arrow_file(&batch, None)),
            (
                "lz4",
                fixtures::arrow_file(&batch, Some(CompressionType::LZ4_FRAME)),
            ),
            (
                "zstd",
                fixtures::arrow_file(&batch, Some(CompressionType::ZSTD)),
            ),
            ("stream", fixtures::arrow_stream(&batch)),
        ] {
            let (t, _d) = open(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(columns(&t), sample_columns(), "{name}");
            assert_eq!(collect(&t, None).unwrap(), sample_rows(), "{name}");
            assert_eq!(t.row_count(), None);
        }
    }

    #[test]
    fn projection_picks_columns() {
        let (t, _d) = open(&fixtures::arrow_file(&fixtures::sample_batch(), None)).unwrap();
        assert_eq!(collect(&t, Some(&[0, 1])).unwrap().len(), 3);
        assert_eq!(
            collect(&t, Some(&[1])).unwrap()[0],
            vec![serde_json::json!("Ana")]
        );
    }

    #[test]
    fn damaged_files_are_one_sentence() {
        let good = fixtures::arrow_file(&fixtures::sample_batch(), None);
        let stream = fixtures::arrow_stream(&fixtures::sample_batch());
        for (name, bytes) in [
            ("short", good[..20].to_vec()),
            ("empty", Vec::new()),
            ("garbage", b"ARROW1\0\0 not really".to_vec()),
        ] {
            let err = open(&bytes)
                .err()
                .unwrap_or_else(|| panic!("{name} opened"));
            assert!(
                err.contains("is not a readable Arrow file"),
                "{name}: {err}"
            );
        }
        // A stream cut inside a batch opens (the schema is intact) and fails when read.
        let (t, _d) = open(&stream[..stream.len() - 200]).unwrap();
        assert!(t.scan(None).any(|b| b.is_err()));
    }
}
