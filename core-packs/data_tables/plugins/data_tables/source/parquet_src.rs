//! Parquet files: schema and row count from the footer, and a scan that reads
//! only the columns a query names, one row group at a time.
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::datatypes::SchemaRef;
use parquet::arrow::ProjectionMask;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::errors::ParquetError;
use parquet::file::metadata::ParquetMetaData;
use parquet::file::reader::{ChunkReader, Length};

use crate::Res;
use crate::table::{BATCH_ROWS, Batches, ScanError, TableSource, file_fingerprint};

/// The most compressed bytes of the columns a query reads that one row group may hold.
const MAX_ROW_GROUP_BYTES: i64 = 256 * 1024 * 1024;

/// A Parquet file the reader opens by path for every read. The reader's own `File` support clones
/// the file handle, which the WebAssembly host does not offer.
struct PathFile {
    path: PathBuf,
    len: u64,
}

impl PathFile {
    fn open(path: &Path) -> std::io::Result<Self> {
        Ok(Self {
            path: path.to_path_buf(),
            len: std::fs::metadata(path)?.len(),
        })
    }

    fn at(&self, start: u64) -> std::io::Result<File> {
        let mut f = File::open(&self.path)?;
        f.seek(SeekFrom::Start(start))?;
        Ok(f)
    }
}

impl Length for PathFile {
    fn len(&self) -> u64 {
        self.len
    }
}

impl ChunkReader for PathFile {
    type T = BufReader<std::io::Take<File>>;

    fn get_read(&self, start: u64) -> Result<Self::T, ParquetError> {
        let take = self.at(start)?.take(self.len.saturating_sub(start));
        Ok(BufReader::with_capacity(64 * 1024, take))
    }

    fn get_bytes(&self, start: u64, length: usize) -> Result<bytes::Bytes, ParquetError> {
        // The buffer grows with what the file really holds, not with a length a damaged footer names.
        let mut buf = Vec::with_capacity(length.min(1 << 20));
        let read = self.at(start)?.take(length as u64).read_to_end(&mut buf)?;
        if read != length {
            return Err(ParquetError::EOF(format!(
                "expected {length} bytes at offset {start}, found {read}"
            )));
        }
        Ok(bytes::Bytes::from(buf))
    }
}

/// A Parquet file as a table.
pub struct ParquetTable {
    path: PathBuf,
    schema: SchemaRef,
    rows: u64,
    meta: Arc<ParquetMetaData>,
    fp: u64,
}

fn corrupt(path: &Path, why: &dyn std::fmt::Display) -> String {
    format!(
        "{} is not a readable Parquet file ({why}); check it was fully written",
        path.display()
    )
}

impl ParquetTable {
    /// Reads the footer of `path`.
    pub fn open(path: &Path) -> Res<Self> {
        let file =
            PathFile::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let builder =
            ParquetRecordBatchReaderBuilder::try_new(file).map_err(|e| corrupt(path, &e))?;
        let meta = Arc::clone(builder.metadata());
        let rows = u64::try_from(meta.file_metadata().num_rows()).unwrap_or(0);
        Ok(Self {
            path: path.to_path_buf(),
            schema: Arc::clone(builder.schema()),
            rows,
            meta,
            fp: file_fingerprint(path)?,
        })
    }
}

impl TableSource for ParquetTable {
    fn schema(&self) -> SchemaRef {
        Arc::clone(&self.schema)
    }

    fn row_count(&self) -> Option<u64> {
        Some(self.rows)
    }

    fn fingerprint(&self) -> u64 {
        self.fp
    }

    fn scan(&self, projection: Option<&[usize]>) -> Batches {
        let fail = |e: String| -> Batches { Box::new(std::iter::once(Err(ScanError::Failed(e)))) };
        let file = match PathFile::open(&self.path) {
            Ok(f) => f,
            Err(e) => return fail(format!("cannot read {}: {e}", self.path.display())),
        };
        let builder = match ParquetRecordBatchReaderBuilder::try_new(file) {
            Ok(b) => b,
            Err(e) => return fail(corrupt(&self.path, &e)),
        };
        let all: Vec<usize> = (0..self.schema.fields().len()).collect();
        let roots = projection.map_or(all, <[usize]>::to_vec);
        let mask = ProjectionMask::roots(builder.parquet_schema(), roots.iter().copied());
        for (i, group) in self.meta.row_groups().iter().enumerate() {
            let bytes: i64 = group
                .columns()
                .iter()
                .filter(|c| {
                    roots.iter().any(|&r| {
                        c.column_path().parts().first().is_some_and(|p| {
                            Some(p.as_str()) == Some(self.schema.field(r).name().as_str())
                        })
                    })
                })
                .map(|c| c.compressed_size())
                .sum();
            if bytes > MAX_ROW_GROUP_BYTES {
                return fail(format!(
                    "row group {} of {} holds {} MiB in the columns read, over the {} MiB one row group may take; select fewer columns or rewrite the file with smaller row groups",
                    i + 1,
                    self.path.display(),
                    bytes / 1024 / 1024,
                    MAX_ROW_GROUP_BYTES / 1024 / 1024
                ));
            }
        }
        match builder
            .with_projection(mask)
            .with_batch_size(BATCH_ROWS)
            .build()
        {
            Ok(reader) => Box::new(reader.map(|r| {
                r.map_err(|e| ScanError::Failed(format!("a Parquet page could not be read: {e}")))
            })),
            Err(e) => fail(corrupt(&self.path, &e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::ScanError;
    use crate::testkit::{Dir, collect, columns, fixtures, sample_columns, sample_rows};
    use parquet::basic::Compression;
    use serde_json::json;

    fn open(bytes: &[u8]) -> Result<(ParquetTable, Dir), String> {
        let dir = Dir::new();
        let p = dir.put("t.parquet", bytes);
        ParquetTable::open(&p).map(|t| (t, dir))
    }

    #[test]
    fn every_codec_reads_back_the_same_rows() {
        let batch = fixtures::sample_batch();
        for codec in [
            Compression::UNCOMPRESSED,
            Compression::SNAPPY,
            Compression::GZIP(Default::default()),
            Compression::BROTLI(Default::default()),
            Compression::LZ4_RAW,
            Compression::ZSTD(Default::default()),
        ] {
            let (t, _d) = open(&fixtures::parquet(&batch, codec)).unwrap();
            assert_eq!(columns(&t), sample_columns(), "{codec:?}");
            assert_eq!(t.row_count(), Some(3));
            assert_eq!(collect(&t, None).unwrap(), sample_rows(), "{codec:?}");
        }
    }

    /// `tests/fixtures/formats/people_zstd.parquet` was written with the reference zstd library, so this
    /// reads another encoder's frames with the pure Rust decoder the plugin links.
    #[test]
    fn a_file_compressed_by_the_reference_zstd_library_reads_back() {
        let (t, _d) = open(include_bytes!(
            "../tests/fixtures/formats/people_zstd.parquet"
        ))
        .unwrap();
        assert_eq!(columns(&t), sample_columns());
        assert_eq!(collect(&t, None).unwrap(), sample_rows());
    }

    #[test]
    fn only_the_projected_columns_are_returned_and_none_still_counts_rows() {
        let (t, _d) = open(&fixtures::parquet(
            &fixtures::sample_batch(),
            Compression::SNAPPY,
        ))
        .unwrap();
        assert_eq!(
            collect(&t, Some(&[1, 4])).unwrap(),
            vec![
                vec![json!("Ana"), json!("2024-02-29")],
                vec![json!(""), json!(null)],
                vec![json!(null), json!("1970-01-01")]
            ]
        );
        let n: usize = t.scan(Some(&[])).map(|b| b.unwrap().num_rows()).sum();
        assert_eq!(n, 3);
    }

    #[test]
    fn a_corrupt_or_truncated_or_foreign_file_is_one_sentence() {
        let good = fixtures::parquet(&fixtures::sample_batch(), Compression::SNAPPY);
        let mut bad_footer = good.clone();
        let n = bad_footer.len();
        bad_footer[n - 40..n - 8].fill(0xAB);
        for (name, bytes) in [
            ("footer", bad_footer),
            ("truncated", good[..60].to_vec()),
            ("text", b"name,score\nAna,9\n".to_vec()),
            ("empty", Vec::new()),
        ] {
            let err = open(&bytes)
                .err()
                .unwrap_or_else(|| panic!("{name} opened"));
            assert!(
                err.contains("is not a readable Parquet file"),
                "{name}: {err}"
            );
            assert!(!err.contains('\n'), "{name}");
        }
    }

    #[test]
    fn a_damaged_data_page_fails_the_scan_with_a_sentence() {
        let good = fixtures::parquet(&fixtures::sample_batch(), Compression::UNCOMPRESSED);
        let (t, _d) = open(&good).unwrap();
        let mut damaged = good.clone();
        // Smash the first row group's data, leaving the footer readable.
        damaged[4..120].fill(0xFF);
        let (broken, _d2) = open(&damaged).unwrap();
        let failed = broken
            .scan(None)
            .any(|b| matches!(b, Err(ScanError::Failed(m)) if m.contains("Parquet")));
        assert!(failed);
        assert!(t.scan(None).all(|b| b.is_ok()));
    }

    #[test]
    fn a_large_file_streams_in_batches_of_the_batch_size() {
        let (t, _d) = open(&fixtures::parquet(
            &fixtures::ints(20_000),
            Compression::SNAPPY,
        ))
        .unwrap();
        assert_eq!(t.row_count(), Some(20_000));
        let sizes: Vec<usize> = t.scan(Some(&[1])).map(|b| b.unwrap().num_rows()).collect();
        assert!(
            sizes.iter().all(|&n| n <= crate::table::BATCH_ROWS),
            "{sizes:?}"
        );
        assert_eq!(sizes.iter().sum::<usize>(), 20_000);
    }
}
