//! Parser for the file-I/O journal emitted by the `LD_PRELOAD` shim.
//!
//! The shim writes one tab-separated ASCII record per line (see
//! `preload_shim.rs` for the format). Parsing resolves every record's file
//! descriptor back to a path by replaying the `open`/`close` stream in
//! sequence order, so downstream code never has to track descriptors.
//!
//! # A journal is whole records or it is refused
//!
//! Every record is [`RECORD_BYTES`] long, a length the shim chose so that a
//! kill cannot cut one (`preload_shim.rs` says how). A journal therefore
//! holds whole records and nothing else, and the parser holds it to that:
//! a line of any other length, a line that is not eight fields, and bytes
//! after the last line end each count in [`Journal::malformed`], which
//! refuses the journal. Nothing that is left of a cut record is ever read as
//! a record: its last number or its path may be missing digits or letters,
//! and a write of 100 bytes would be replayed as a write of 1.
//!
//! An operation whose record is missing because the child died first is one
//! that was in flight: the kernel may have finished the call, but the thread
//! never left the shim, so the engine never saw the call return and told no
//! caller anything that rests on it. A power cut may undo such an operation,
//! and the reconstruction, which replays only what is recorded, does.

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The length of every record, line end included. The shim's `RECORD`.
pub const RECORD_BYTES: usize = 512;

/// Raw `open(2)` flag bits the reconstruction cares about.
pub const O_CREAT: i64 = 0o100;
pub const O_TRUNC: i64 = 0o1000;
pub const O_APPEND: i64 = 0o2000;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum OpKind {
    Open,
    Write,
    Sync,
    Truncate,
    Close,
    RenameFrom,
    RenameTo,
    Unlink,
}

impl OpKind {
    fn from_tag(tag: u8, a: i64) -> Option<OpKind> {
        Some(match tag {
            b'O' => OpKind::Open,
            b'W' => OpKind::Write,
            b'S' => OpKind::Sync,
            b'T' => OpKind::Truncate,
            b'C' => OpKind::Close,
            b'R' if a == 0 => OpKind::RenameFrom,
            b'R' => OpKind::RenameTo,
            b'U' => OpKind::Unlink,
            _ => return None,
        })
    }
}

/// One recorded file-I/O operation, with its descriptor already resolved
/// to the path it was opened on.
#[derive(Clone, Debug)]
pub struct Record {
    pub seq: u64,
    /// Kernel thread id. Distinguishes a foreground write from a
    /// background flush or compaction write.
    pub tid: i64,
    pub kind: OpKind,
    pub fd: i64,
    /// `Open`: file size at open. `Write`: byte offset, `-1` for a write
    /// on an `O_APPEND` descriptor. `Truncate`: the new length.
    /// `Sync`: 0 for `fsync`, 1 for `fdatasync`.
    pub a: i64,
    /// `Open`: raw open flags. `Write`: requested byte count.
    pub b: i64,
    /// Return value of the real call. Negative means it failed.
    pub ret: i64,
    pub path: PathBuf,
}

impl Record {
    /// A call that reported success. A failed write moved no bytes and
    /// must not contribute to the reconstruction.
    pub fn succeeded(&self) -> bool {
        match self.kind {
            OpKind::Write => self.ret > 0,
            OpKind::Open => self.fd >= 0,
            _ => self.ret >= 0,
        }
    }

    /// Bytes actually transferred by a write, 0 for any other kind.
    pub fn written(&self) -> u64 {
        if self.kind == OpKind::Write && self.ret > 0 {
            self.ret as u64
        } else {
            0
        }
    }

    pub fn opened_appending(&self) -> bool {
        self.kind == OpKind::Open && self.b & O_APPEND != 0
    }

    pub fn opened_creating(&self) -> bool {
        self.kind == OpKind::Open && self.b & O_CREAT != 0
    }

    pub fn opened_truncating(&self) -> bool {
        self.kind == OpKind::Open && self.b & O_TRUNC != 0
    }
}

/// The recorded syscall stream of one child process, in sequence order.
#[derive(Clone, Debug, Default)]
pub struct Journal {
    pub records: Vec<Record>,
    pub source: PathBuf,
    /// Lines the parser could not understand. Non-empty means the journal
    /// is not trustworthy and callers must fail rather than proceed.
    pub malformed: usize,
}

/// The journal path this harness uses for a database directory. It lives
/// beside the directory rather than inside it, so the recorder never
/// records itself and a power-loss reconstruction never rewrites it.
pub fn journal_path_for(db_dir: &Path) -> PathBuf {
    let name = db_dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "db".to_string());
    db_dir.with_file_name(format!("{name}.faultjournal"))
}

/// The substring the shim matches paths against for this database. The
/// trailing separator is what keeps `<db>.faultjournal` and `<db>.acks`
/// out of the recording.
pub fn root_filter_for(db_dir: &Path) -> String {
    format!("{}{}", db_dir.display(), std::path::MAIN_SEPARATOR)
}

impl Journal {
    /// Parse a journal file. Missing file yields an empty journal, which
    /// callers must treat as "the shim never ran" rather than "the child
    /// did no I/O".
    pub fn read(path: &Path) -> io::Result<Journal> {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e),
        };
        Ok(Journal::parse(&text, path))
    }

    fn parse(text: &str, source: &Path) -> Journal {
        struct Raw {
            seq: u64,
            tid: i64,
            tag: u8,
            fd: i64,
            a: i64,
            b: i64,
            ret: i64,
            path: String,
        }
        let mut raw: Vec<Raw> = Vec::new();
        // What follows the last line end is a record that was cut.
        let (whole, cut) = match text.rfind('\n') {
            Some(end) => text.split_at(end + 1),
            None => ("", text),
        };
        let mut malformed = usize::from(!cut.is_empty());
        for line in whole.lines() {
            let parsed = (|| {
                if line.len() + 1 != RECORD_BYTES {
                    return None;
                }
                let mut f = line.trim_end_matches('\0').split('\t');
                let seq: u64 = f.next()?.parse().ok()?;
                let tid: i64 = f.next()?.parse().ok()?;
                let tag = *f.next()?.as_bytes().first()?;
                let fd: i64 = f.next()?.parse().ok()?;
                let a: i64 = f.next()?.parse().ok()?;
                let b: i64 = f.next()?.parse().ok()?;
                let ret: i64 = f.next()?.parse().ok()?;
                let path = f.next()?.to_string();
                if f.next().is_some() {
                    return None;
                }
                Some(Raw {
                    seq,
                    tid,
                    tag,
                    fd,
                    a,
                    b,
                    ret,
                    path,
                })
            })();
            match parsed {
                Some(p) => raw.push(p),
                None => malformed += 1,
            }
        }
        raw.sort_by_key(|r| r.seq);

        let mut fds: HashMap<i64, PathBuf> = HashMap::new();
        // The shim reads a write's offset with a `SEEK_CUR` query, which is
        // exact for a plain descriptor but reads 0 for the first write on an
        // `O_APPEND` descriptor: the kernel only moves that descriptor to the
        // end of the file at write time. Recording 0 would mark durable bytes
        // at the start of an existing file as unsynced, and a power-loss
        // reconstruction would then discard a whole file that survived. Every
        // write on an appending descriptor is therefore normalised to `-1`,
        // which is what `Record::a` documents and what the reconstruction
        // already resolves against the running end of file.
        let mut appending: HashMap<i64, bool> = HashMap::new();
        let mut records = Vec::with_capacity(raw.len());
        for Raw {
            seq,
            tid,
            tag,
            fd,
            a,
            b,
            ret,
            path,
        } in raw
        {
            let kind = match OpKind::from_tag(tag, a) {
                Some(k) => k,
                None => {
                    malformed += 1;
                    continue;
                }
            };
            let mut a = a;
            let resolved = match kind {
                OpKind::Open => {
                    let p = PathBuf::from(&path);
                    if fd >= 0 {
                        fds.insert(fd, p.clone());
                        appending.insert(fd, b & O_APPEND != 0);
                    }
                    p
                }
                OpKind::RenameFrom | OpKind::RenameTo | OpKind::Unlink => PathBuf::from(&path),
                _ => {
                    if kind == OpKind::Write && appending.get(&fd).copied().unwrap_or(false) {
                        a = -1;
                    }
                    fds.get(&fd).cloned().unwrap_or_default()
                }
            };
            if kind == OpKind::Close {
                fds.remove(&fd);
                appending.remove(&fd);
            }
            records.push(Record {
                seq,
                tid,
                kind,
                fd,
                a,
                b,
                ret,
                path: resolved,
            });
        }
        Journal {
            records,
            source: source.to_path_buf(),
            malformed,
        }
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Highest sequence number recorded, i.e. the last thing the child did
    /// before it died.
    pub fn last_seq(&self) -> u64 {
        self.records.last().map(|r| r.seq).unwrap_or(0)
    }

    /// Sequence numbers of every successful `fsync`/`fdatasync`, in order.
    pub fn sync_seqs(&self) -> Vec<u64> {
        self.records
            .iter()
            .filter(|r| r.kind == OpKind::Sync && r.succeeded())
            .map(|r| r.seq)
            .collect()
    }

    /// Successful writes whose path contains `needle`.
    pub fn writes_to(&self, needle: &str) -> Vec<&Record> {
        self.records
            .iter()
            .filter(|r| {
                r.kind == OpKind::Write
                    && r.succeeded()
                    && r.path.to_string_lossy().contains(needle)
            })
            .collect()
    }

    /// Successful syncs whose path contains `needle`.
    pub fn syncs_to(&self, needle: &str) -> Vec<&Record> {
        self.records
            .iter()
            .filter(|r| {
                r.kind == OpKind::Sync && r.succeeded() && r.path.to_string_lossy().contains(needle)
            })
            .collect()
    }

    /// Every thread that performed a write. More than one means a
    /// background flush or compaction was writing concurrently.
    pub fn writing_tids(&self) -> BTreeSet<i64> {
        self.records
            .iter()
            .filter(|r| r.kind == OpKind::Write && r.succeeded())
            .map(|r| r.tid)
            .collect()
    }

    /// True when some thread other than `main_tid` wrote an SSTable, which
    /// is how a test confirms a background flush or compaction was really
    /// in flight at the crash point instead of assuming it.
    pub fn background_sst_write_seen(&self, main_tid: i64) -> bool {
        self.records.iter().any(|r| {
            r.kind == OpKind::Write
                && r.succeeded()
                && r.tid != main_tid
                && r.path.extension().and_then(|s| s.to_str()) == Some("sst")
        })
    }

    /// Thread id that issued the first recorded write, which for the
    /// built-in workloads is the foreground writer.
    pub fn first_writer_tid(&self) -> Option<i64> {
        self.records
            .iter()
            .find(|r| r.kind == OpKind::Write && r.succeeded())
            .map(|r| r.tid)
    }

    /// Records up to and including `seq`.
    pub fn upto(&self, seq: u64) -> impl Iterator<Item = &Record> {
        self.records.iter().filter(move |r| r.seq <= seq)
    }
}

impl fmt::Display for Journal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "journal {} ({} records, {} malformed, last seq {})",
            self.source.display(),
            self.records.len(),
            self.malformed,
            self.last_seq(),
        )?;
        let mut per_path: Vec<(String, usize, u64, usize)> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        for r in &self.records {
            let key = r.path.to_string_lossy().into_owned();
            let i = *index.entry(key.clone()).or_insert_with(|| {
                per_path.push((key, 0, 0, 0));
                per_path.len() - 1
            });
            match r.kind {
                OpKind::Write if r.succeeded() => {
                    per_path[i].1 += 1;
                    per_path[i].2 += r.written();
                }
                OpKind::Sync if r.succeeded() => per_path[i].3 += 1,
                _ => {}
            }
        }
        per_path.sort();
        for (path, writes, bytes, syncs) in per_path {
            if writes == 0 && syncs == 0 {
                continue;
            }
            writeln!(
                f,
                "  {writes:>5} writes {bytes:>9} bytes {syncs:>4} syncs  {path}"
            )?;
        }
        Ok(())
    }
}
