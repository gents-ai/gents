//! Acknowledgement files: what a crash child was told, for its parent to
//! check against what survived.
//!
//! A child appends a record to its acknowledgement file after a call the
//! engine returned from, and the parent then checks that every acknowledged
//! operation survived the crash. The file sits beside the database directory
//! (`ChildSpec::ack_path`), outside what the shim records and what a
//! power-loss reconstruction rewrites, so it holds what the child wrote.
//!
//! # Every record is whole
//!
//! The child is killed while its threads write. The kernel copies a write a
//! page at a time and gives up between two pages once the kill is pending, so
//! a record that lies across a page boundary can be left cut at the boundary,
//! and a thread the kill has not reached yet can append a whole record right
//! behind the cut one, on the same line. With records written as lines of
//! their own length, four threads acknowledging as fast as they could left a
//! cut line in 23 of 6,000 kills, 4 of them with a whole line run onto it.
//! The readers then panicked on an `unwrap` that did not say why, read a cut
//! number as a smaller one, or read a cut line and the next as one record of
//! a thread that did not exist.
//!
//! So every record is [`ACK_BYTES`] bytes long: its numbers in decimal,
//! separated by spaces, padded with spaces, with a line end as its last byte.
//! The length divides a page and every write appends whole records, so a page
//! boundary of the file always falls between two records: a kill leaves a
//! record whole or leaves none of it. [`read_acks`] holds a file to that, and
//! refuses (naming the file) anything that is not whole records of the shape
//! the caller asked for.
//!
//! # What a missing record means
//!
//! A record is written after the engine returned. An operation whose record
//! the child did not get to write was not acknowledged, so no check requires
//! it to survive. Dropping such a record would not make a check stricter: it
//! would leave out one operation the child had not acknowledged. Here
//! nothing is dropped. A cut record cannot arise, and if one is ever found
//! the file is refused, because then the harness is not doing what this
//! module says it does.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The length of every record, line end included. It divides a page.
pub const ACK_BYTES: usize = 32;

/// A child's handle on its acknowledgement file. Each thread that
/// acknowledges opens its own; every write appends whole records.
pub struct Acks {
    file: File,
    path: PathBuf,
}

impl Acks {
    /// Open `path` to append acknowledgements to, creating it.
    pub fn open(path: &Path) -> Acks {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap_or_else(|e| panic!("opening the acknowledgement file {}: {e}", path.display()));
        Acks {
            file,
            path: path.to_path_buf(),
        }
    }

    /// Acknowledge one operation, as one record written with one call.
    pub fn ack<const N: usize>(&mut self, record: [u64; N]) {
        self.ack_all([record]);
    }

    /// Acknowledge several operations with one write. A kill can stop the
    /// write only between two records.
    pub fn ack_all<const N: usize>(&mut self, records: impl IntoIterator<Item = [u64; N]>) {
        let mut bytes = Vec::new();
        for record in records {
            let start = bytes.len();
            for (n, value) in record.iter().enumerate() {
                let space = if n == 0 { "" } else { " " };
                write!(bytes, "{space}{value}").expect("writing to a Vec cannot fail");
            }
            assert!(
                bytes.len() - start < ACK_BYTES,
                "the acknowledgement {record:?} does not fit in a record of {ACK_BYTES} bytes"
            );
            bytes.resize(start + ACK_BYTES - 1, b' ');
            bytes.push(b'\n');
        }
        self.file.write_all(&bytes).unwrap_or_else(|e| {
            panic!(
                "writing to the acknowledgement file {}: {e}",
                self.path.display()
            )
        });
    }
}

/// The records of an acknowledgement file, read whole: what a child was
/// told, in the order its threads appended it.
#[derive(Clone, Debug)]
pub struct Acked {
    path: PathBuf,
    records: Vec<Vec<u64>>,
}

impl Acked {
    /// Every record as the `N` numbers the child wrote into it. Panics,
    /// naming the file, if a record holds another count.
    pub fn of<const N: usize>(&self) -> Vec<[u64; N]> {
        self.records
            .iter()
            .enumerate()
            .map(|(n, record)| {
                <[u64; N]>::try_from(&record[..]).unwrap_or_else(|_| {
                    panic!(
                        "the acknowledgement file {} holds a record at byte {} of {} numbers, \
                         not {N}: {record:?}",
                        self.path.display(),
                        n * ACK_BYTES,
                        record.len(),
                    )
                })
            })
            .collect()
    }

    /// How many operations the child acknowledged.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

/// Every record of the acknowledgement file at `path`; none when the child
/// never wrote one. Panics, naming the file, unless the file is whole
/// records.
pub fn read_acks(path: &Path) -> Acked {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => panic!("reading the acknowledgement file {}: {e}", path.display()),
    };
    let whole = bytes.len() - bytes.len() % ACK_BYTES;
    assert_eq!(
        whole,
        bytes.len(),
        "the acknowledgement file {} ends in {} bytes that are not a whole record: {:?}",
        path.display(),
        bytes.len() - whole,
        String::from_utf8_lossy(&bytes[whole..]),
    );
    let records = bytes
        .chunks(ACK_BYTES)
        .enumerate()
        .map(|(n, record)| {
            parse(record).unwrap_or_else(|| {
                panic!(
                    "the acknowledgement file {} holds a record at byte {} that is not numbers \
                     padded to a line end: {:?}",
                    path.display(),
                    n * ACK_BYTES,
                    String::from_utf8_lossy(record),
                )
            })
        })
        .collect();
    Acked {
        path: path.to_path_buf(),
        records,
    }
}

/// The numbers of one record, if it is one.
fn parse(record: &[u8]) -> Option<Vec<u64>> {
    let (&b'\n', text) = record.split_last()? else {
        return None;
    };
    let text = std::str::from_utf8(text).ok()?.trim_end_matches(' ');
    if text.is_empty() {
        return None;
    }
    text.split(' ').map(|number| number.parse().ok()).collect()
}
