//! Offline SSTable builder and bulk-ingest options.
//!
//! [`SstFileWriter`] lets a caller construct a valid regolith SSTable on disk
//! without going through a [`crate::Db`], and
//! [`crate::Db::ingest_external_files`] bulk-loads those files into a
//! running database. All entries in a single ingest file are assigned
//! one freshly-allocated sequence number at ingest time. The file is
//! installed as it is, copied or linked: the database records that
//! sequence beside the file and reads every entry at it, so the
//! placeholder seq (`0`) this writer embeds is never read.

use std::path::{Path, PathBuf};

use crate::column_family::{ColumnFamilyHandle, DEFAULT_CF_ID, prefix_key};
use crate::engine::internal_key::{VALUE_TYPE_DELETION, VALUE_TYPE_VALUE, encode_internal_key};
use crate::engine::sstable::SsTableWriter;
use crate::options::Options;

/// Writes a standalone SSTable file that a running [`crate::Db`] can
/// bulk-ingest via [`crate::Db::ingest_external_files`].
///
/// Keys must be supplied in **strictly ascending** user-key order -
/// duplicates and out-of-order keys are rejected with an error. Every
/// entry is written with a placeholder sequence number of `0`; the
/// real sequence number is assigned by the engine when the file is
/// ingested, and every entry reads at it.
pub struct SstFileWriter {
    inner: SsTableWriter,
    path: PathBuf,
    last_user_key: Option<Vec<u8>>,
    num_entries: u64,
    max_key_size: usize,
    max_value_size: usize,
}

/// Summary of a finished ingest file, returned by [`SstFileWriter::finish`].
#[derive(Debug, Clone)]
pub struct SstFileMeta {
    /// Path the file was written to.
    pub path: PathBuf,
    /// Lower bound of the file in storage order. A file can hold entries of
    /// several column families ([`SstFileWriter::put_cf`]), so this is not
    /// a key as the caller wrote it: it is the 4-byte big-endian id of a
    /// column family, then the caller's key in that family.
    pub smallest_user_key: Vec<u8>,
    /// Upper bound of the file in storage order, in the form of
    /// [`SstFileMeta::smallest_user_key`].
    pub largest_user_key: Vec<u8>,
    /// Number of point entries written (puts + deletes).
    pub num_entries: u64,
}

/// Options controlling how [`crate::Db::ingest_external_files`] moves
/// files into the database.
///
/// Unless `ingest_behind` is set, a file goes to the deepest level such
/// that no L0 table holds a key of the file's range and no table of that
/// level or any level above it overlaps the range; with no such level it
/// goes in front of every L0 table. The range includes the file's range
/// tombstones.
#[derive(Debug, Clone, Copy)]
pub struct IngestOptions {
    /// Move each file into the database instead of copying it.
    ///
    /// With `true`, the file is hard-linked into the database's table
    /// directory where the environment has hard links (it is copied
    /// where it has none, or where the link fails, across filesystems
    /// say), and its source path is removed once it is installed. The
    /// caller hands the file over: it must not write to it afterwards.
    /// A source whose removal fails is left in place and logged, since
    /// the ingest itself succeeded; so is one a crash catches between
    /// the install and the removal.
    ///
    /// With `false`, the default, the file is copied and the source is
    /// left untouched: the caller may rewrite, delete or re-ingest it.
    pub move_files: bool,
    /// Reject the ingest if any live snapshot is pinned. Ingest
    /// assigns a single new seq to every entry in the file; a snapshot
    /// taken before the ingest would otherwise be inconsistent with
    /// that seq's apparent ordering.
    pub snapshot_consistency: bool,
    /// Force every ingest file to land at the bottommost level. The
    /// ingest is rejected if any input file's user-key range overlaps
    /// an existing SSTable at any level, or a memtable holds a key in
    /// it.
    pub ingest_behind: bool,
}

impl Default for IngestOptions {
    fn default() -> Self {
        Self {
            move_files: false,
            snapshot_consistency: true,
            ingest_behind: false,
        }
    }
}

impl SstFileWriter {
    /// Create a new SSTable file at `path`. The file is overwritten if
    /// it already exists. The writer borrows `block_size`,
    /// `bloom_bits_per_key`, and `compression` from `opts` after
    /// validating the option invariants.
    pub fn create<P: AsRef<Path>>(path: P, opts: &Options) -> crate::Result<Self> {
        opts.validate()?;
        let path = path.as_ref().to_path_buf();
        let inner = SsTableWriter::new_in(
            &opts.env,
            &path,
            opts.block_size,
            opts.bloom_bits_per_key,
            opts.compression,
            opts.prefix_extractor.clone(),
            opts.partitioned_index,
            opts.metadata_block_size,
            opts.keyring().as_deref(),
        )
        .map_err(crate::Error::from)?;
        Ok(Self {
            inner,
            path,
            last_user_key: None,
            num_entries: 0,
            max_key_size: opts.max_key_size,
            max_value_size: opts.max_value_size,
        })
    }

    /// Append a `(key, value)` pair to the default column family.
    /// `key` must be strictly greater than every previously added
    /// key.
    pub fn put(&mut self, key: &[u8], value: &[u8]) -> crate::Result<()> {
        let prefixed = prefix_key(DEFAULT_CF_ID, key);
        self.add(&prefixed, value, VALUE_TYPE_VALUE)
    }

    /// Append a deletion tombstone for `key` in the default column
    /// family.
    pub fn delete(&mut self, key: &[u8]) -> crate::Result<()> {
        let prefixed = prefix_key(DEFAULT_CF_ID, key);
        self.add(&prefixed, &[], VALUE_TYPE_DELETION)
    }

    /// Append a `(key, value)` pair scoped to column family `cf`.
    /// Keys across CFs must still arrive in strictly ascending
    /// order (by prefixed bytes).
    pub fn put_cf(
        &mut self,
        cf: &ColumnFamilyHandle,
        key: &[u8],
        value: &[u8],
    ) -> crate::Result<()> {
        let prefixed = prefix_key(cf.id(), key);
        self.add(&prefixed, value, VALUE_TYPE_VALUE)
    }

    /// Append a deletion tombstone scoped to column family `cf`.
    pub fn delete_cf(&mut self, cf: &ColumnFamilyHandle, key: &[u8]) -> crate::Result<()> {
        let prefixed = prefix_key(cf.id(), key);
        self.add(&prefixed, &[], VALUE_TYPE_DELETION)
    }

    fn add(&mut self, key: &[u8], value: &[u8], value_type: u8) -> crate::Result<()> {
        let user_key_len = key.len().saturating_sub(4);
        if user_key_len > self.max_key_size {
            return Err(crate::Error::invalid_argument(format!(
                "key length {} exceeds configured max_key_size {}",
                user_key_len, self.max_key_size
            )));
        }
        if value.len() > self.max_value_size {
            return Err(crate::Error::invalid_argument(format!(
                "value length {} exceeds configured max_value_size {}",
                value.len(),
                self.max_value_size
            )));
        }
        if let Some(last) = &self.last_user_key
            && key <= last.as_slice()
        {
            return Err(crate::Error::invalid_argument(
                "SstFileWriter keys must arrive in strictly ascending order",
            ));
        }
        let internal = encode_internal_key(key, 0, value_type);
        self.inner
            .add(&internal, value)
            .map_err(crate::Error::from)?;
        self.last_user_key = Some(key.to_vec());
        self.num_entries += 1;
        Ok(())
    }

    /// Finalize the file. Errors if no entries were written - an empty
    /// ingest file is almost certainly a bug and would be rejected at
    /// ingest time anyway.
    pub fn finish(self) -> crate::Result<SstFileMeta> {
        let Self { inner, path, .. } = self;
        let summary = inner.finish().map_err(crate::Error::from)?.ok_or_else(|| {
            crate::Error::invalid_argument("SstFileWriter::finish called with no entries")
        })?;
        Ok(SstFileMeta {
            path,
            smallest_user_key: summary.smallest_user_key,
            largest_user_key: summary.largest_user_key,
            num_entries: summary.num_entries,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Db, Options};
    use tempfile::TempDir;

    fn open_tmp() -> (Db, TempDir) {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), Options::default()).unwrap();
        (db, dir)
    }

    fn build_sst(path: &Path, entries: &[(&[u8], Option<&[u8]>)]) -> SstFileMeta {
        let mut w = SstFileWriter::create(path, &Options::default()).unwrap();
        for (k, v) in entries {
            match v {
                Some(v) => w.put(k, v).unwrap(),
                None => w.delete(k).unwrap(),
            }
        }
        w.finish().unwrap()
    }

    #[test]
    fn test_create_rejects_invalid_options() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bad-options.sst");
        match SstFileWriter::create(&path, &Options::default().block_size(0)) {
            Ok(_) => panic!("expected invalid options error"),
            Err(crate::Error::InvalidArgument(message)) => assert!(message.contains("block_size")),
            Err(other) => panic!("expected invalid argument, got {other:?}"),
        }
    }

    #[test]
    fn test_put_out_of_order_rejected() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bad.sst");
        let mut w = SstFileWriter::create(&path, &Options::default()).unwrap();
        w.put(b"b", b"1").unwrap();
        assert!(w.put(b"a", b"2").is_err());
        assert!(w.put(b"b", b"3").is_err());
    }

    #[test]
    fn test_put_and_delete_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("rt.sst");
        let meta = build_sst(
            &path,
            &[
                (b"a", Some(b"1")),
                (b"b", Some(b"2")),
                (b"c", None),
                (b"d", Some(b"4")),
            ],
        );
        assert_eq!(meta.num_entries, 4);
        // The meta reports the on-disk CF-prefixed keys; the
        // default CF's prefix is `[0,0,0,1]`.
        assert_eq!(meta.smallest_user_key, vec![0, 0, 0, 1, b'a']);
        assert_eq!(meta.largest_user_key, vec![0, 0, 0, 1, b'd']);
        assert!(meta.path.exists());
    }

    #[test]
    fn test_empty_finish_errors() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("empty.sst");
        let w = SstFileWriter::create(&path, &Options::default()).unwrap();
        match w.finish().unwrap_err() {
            crate::Error::InvalidArgument(message) => assert!(message.contains("no entries")),
            other => panic!("expected invalid argument error, got {other:?}"),
        }
    }

    #[test]
    fn test_configured_key_value_size_limits_are_enforced() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("limited.sst");
        let opts = Options::default().max_key_size(3).max_value_size(4);
        let mut w = SstFileWriter::create(&path, &opts).unwrap();

        w.put(b"abc", b"1234").unwrap();
        assert!(w.put(b"abcd", b"1").is_err());
        assert!(w.put(b"bcd", b"12345").is_err());
    }

    #[test]
    fn test_ingest_into_empty_db() {
        let (db, dir) = open_tmp();
        let sst_path = dir.path().join("external-1.sst");
        build_sst(
            &sst_path,
            &[
                (b"apple", Some(b"red")),
                (b"banana", Some(b"yellow")),
                (b"cherry", Some(b"dark-red")),
            ],
        );

        db.ingest_external_files(&[sst_path], IngestOptions::default())
            .wait()
            .unwrap();

        assert_eq!(db.get(b"apple").unwrap(), Some(b"red".to_vec()));
        assert_eq!(db.get(b"banana").unwrap(), Some(b"yellow".to_vec()));
        assert_eq!(db.get(b"cherry").unwrap(), Some(b"dark-red".to_vec()));

        let scanned = db.scan(None, None).unwrap();
        assert_eq!(scanned.len(), 3);
    }

    #[test]
    fn test_ingest_overlap_lands_above_the_level_it_overlaps() {
        let (db, dir) = open_tmp();
        db.put(b"banana", b"old").unwrap();
        db.compact_range(None, None).wait().unwrap();
        let bottom = crate::engine::manifest::MAX_LEVELS - 1;
        assert_eq!(db.level_file_count(bottom), 1);

        let sst_path = dir.path().join("external-overlap.sst");
        build_sst(
            &sst_path,
            &[(b"apple", Some(b"a")), (b"banana", Some(b"new"))],
        );

        db.ingest_external_files(&[sst_path], IngestOptions::default())
            .wait()
            .unwrap();

        assert_eq!(db.get(b"apple").unwrap(), Some(b"a".to_vec()));
        assert_eq!(db.get(b"banana").unwrap(), Some(b"new".to_vec()));
        // The deepest level above every table the range overlaps.
        assert_eq!(db.level_file_count(bottom - 1), 1);
        assert_eq!(db.level_file_count(bottom), 1);
        assert_eq!(db.level_file_count(0), 0);
    }

    #[test]
    fn test_ingest_disjoint_lands_at_deep_level() {
        let (db, dir) = open_tmp();
        db.put(b"aaa", b"x").unwrap();
        db.put(b"bbb", b"y").unwrap();
        db.compact_range(None, None).wait().unwrap();
        let before_l0 = db.level_file_count(0);

        let sst_path = dir.path().join("external-disjoint.sst");
        build_sst(&sst_path, &[(b"mmm", Some(b"m")), (b"nnn", Some(b"n"))]);

        db.ingest_external_files(&[sst_path], IngestOptions::default())
            .wait()
            .unwrap();

        assert_eq!(db.get(b"mmm").unwrap(), Some(b"m".to_vec()));
        assert_eq!(db.get(b"nnn").unwrap(), Some(b"n".to_vec()));
        // The ingest should NOT have added an L0 file: its range is
        // disjoint from every existing file.
        assert_eq!(db.engine.level_file_count(0), before_l0);
    }

    #[test]
    fn test_ingest_behind_rejects_overlap() {
        let (db, dir) = open_tmp();
        db.put(b"banana", b"old").unwrap();

        let sst_path = dir.path().join("external-behind-bad.sst");
        build_sst(&sst_path, &[(b"banana", Some(b"new"))]);

        let opts = IngestOptions {
            ingest_behind: true,
            ..Default::default()
        };
        assert!(db.ingest_external_files(&[sst_path], opts).wait().is_err());
    }

    #[test]
    fn test_ingest_behind_forces_bottommost() {
        let (db, dir) = open_tmp();
        db.put(b"aaa", b"x").unwrap();
        db.compact_range(None, None).wait().unwrap();

        let sst_path = dir.path().join("external-behind-ok.sst");
        build_sst(&sst_path, &[(b"zzz", Some(b"z"))]);

        let opts = IngestOptions {
            ingest_behind: true,
            ..Default::default()
        };
        db.ingest_external_files(&[sst_path], opts).wait().unwrap();

        assert_eq!(db.get(b"zzz").unwrap(), Some(b"z".to_vec()));
        // Placed at the bottommost level - not L0.
        assert_eq!(db.level_file_count(0), 0);
    }

    #[test]
    fn test_ingest_post_read_and_iter() {
        let (db, dir) = open_tmp();
        let sst_path = dir.path().join("external-iter.sst");
        let entries: Vec<(Vec<u8>, Vec<u8>)> = (0..20u32)
            .map(|i| {
                (
                    format!("key_{:04}", i).into_bytes(),
                    format!("val_{}", i).into_bytes(),
                )
            })
            .collect();
        {
            let mut w = SstFileWriter::create(&sst_path, &Options::default()).unwrap();
            for (k, v) in &entries {
                w.put(k, v).unwrap();
            }
            w.finish().unwrap();
        }

        db.ingest_external_files(&[sst_path], IngestOptions::default())
            .wait()
            .unwrap();

        for (k, v) in &entries {
            assert_eq!(db.get(k).unwrap(), Some(v.clone()));
        }
        let scanned = db.scan(None, None).unwrap();
        assert_eq!(scanned.len(), entries.len());
        for ((k_got, v_got), (k, v)) in scanned.iter().zip(entries.iter()) {
            assert_eq!(k_got, k);
            assert_eq!(v_got, v);
        }
    }

    #[test]
    fn test_ingest_snapshot_consistency_rejects_with_live_snapshot() {
        let (db, dir) = open_tmp();
        db.put(b"a", b"1").unwrap();
        let snap = db.snapshot();

        let sst_path = dir.path().join("external-snap.sst");
        build_sst(&sst_path, &[(b"b", Some(b"2"))]);

        let err = db
            .ingest_external_files(&[sst_path], IngestOptions::default())
            .wait()
            .unwrap_err();
        drop(snap);
        let msg = format!("{err}");
        assert!(msg.contains("snapshot"), "got: {msg}");
    }

    #[test]
    fn test_ingest_snapshot_consistency_false_preserves_old_snapshot_view() {
        let (db, dir) = open_tmp();
        db.put(b"a", b"1").unwrap();
        let snap = db.snapshot();

        let sst_path = dir.path().join("external-snap2.sst");
        build_sst(&sst_path, &[(b"b", Some(b"2"))]);

        let opts = IngestOptions {
            snapshot_consistency: false,
            ..Default::default()
        };
        db.ingest_external_files(&[sst_path], opts).wait().unwrap();

        // Live db sees both.
        assert_eq!(db.get(b"a").unwrap(), Some(b"1".to_vec()));
        assert_eq!(db.get(b"b").unwrap(), Some(b"2".to_vec()));
        // Pre-ingest snapshot sees only the original key - the
        // ingested entry carries a higher seq than the snapshot's.
        assert_eq!(snap.get(b"a").unwrap(), Some(b"1".to_vec()));
        assert_eq!(snap.get(b"b").unwrap(), None);
    }

    #[test]
    fn test_ingest_then_compact_range_merges() {
        let (db, dir) = open_tmp();
        db.put(b"aaa", b"old").unwrap();
        db.compact_range(None, None).wait().unwrap();

        let sst_path = dir.path().join("external-merge.sst");
        build_sst(&sst_path, &[(b"bbb", Some(b"b")), (b"ccc", Some(b"c"))]);
        db.ingest_external_files(&[sst_path], IngestOptions::default())
            .wait()
            .unwrap();

        db.compact_range(None, None).wait().unwrap();

        assert_eq!(db.get(b"aaa").unwrap(), Some(b"old".to_vec()));
        assert_eq!(db.get(b"bbb").unwrap(), Some(b"b".to_vec()));
        assert_eq!(db.get(b"ccc").unwrap(), Some(b"c".to_vec()));
    }
}
