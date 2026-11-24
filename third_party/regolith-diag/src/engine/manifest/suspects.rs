//! The discarded-table guard's search of `sst/`: the unreferenced tables
//! that may hold data beside a manifest a crash left without its header.
//!
//! The directory is walked one entry at a time. The guard needs how many
//! tables it found and the first few by path to name in its refusal, so
//! that is all it keeps: [`SUSPECTS_NAMED`] tables and a count, whatever
//! the directory holds.

use std::path::{Path, PathBuf};

use super::super::seal::Keyring;
use super::super::sstable::table_carries_data;
use crate::env::Env;

/// How many suspects the guard's error message names before summarising
/// the rest. The cap is reported in the message so a long list never
/// reads as a short one.
pub(super) const SUSPECTS_NAMED: usize = 8;

/// An unreferenced `*.sst` file that the discarded-table guard could not
/// dismiss as a crash artifact, with the reason it counts.
#[derive(Debug)]
pub(super) struct SuspectTable {
    path: PathBuf,
    /// `None` when the file's metadata could not be read.
    len: Option<u64>,
    reason: String,
}

/// What the guard found: how many suspects, and the first
/// [`SUSPECTS_NAMED`] of them in path order.
#[derive(Debug, Default)]
pub(super) struct Suspects {
    count: usize,
    named: Vec<SuspectTable>,
}

impl Suspects {
    /// How many suspects were found.
    pub(super) fn count(&self) -> usize {
        self.count
    }

    /// Count `suspect`, and keep it only when it is among the first
    /// [`SUSPECTS_NAMED`] by path seen so far, so the walk holds that many
    /// whatever the directory holds and still names the same ones a full
    /// sort would.
    fn add(&mut self, suspect: SuspectTable) {
        self.count += 1;
        let at = self.named.partition_point(|s| s.path < suspect.path);
        if at < SUSPECTS_NAMED {
            self.named.truncate(SUSPECTS_NAMED - 1);
            self.named.insert(at, suspect);
        }
    }

    /// The named suspects, each with its size and reason, then how many
    /// more there are.
    pub(super) fn describe(&self) -> String {
        let mut out = self
            .named
            .iter()
            .map(|s| match s.len {
                Some(len) => format!("{} ({len} bytes, {})", s.path.display(), s.reason),
                None => format!("{} (size unknown, {})", s.path.display(), s.reason),
            })
            .collect::<Vec<_>>()
            .join(", ");
        if self.count > self.named.len() {
            out.push_str(&format!(
                ", and {} more not named here",
                self.count - self.named.len()
            ));
        }
        out
    }
}

/// The unreferenced `*.sst` files in `sst_dir` that could plausibly hold
/// live data.
///
/// A zero-length table, or one whose footer records no entry and no
/// range tombstone, holds nothing an open could lose, so it is skipped
/// rather than counted, and the skipped ones are logged once.
///
/// Everything else counts, including a file whose footer will not
/// parse. An unreadable file cannot be proved empty, and keeping the
/// database shut preserves it for repair.
///
/// A directory that cannot be walked, from the start or part way, is
/// logged and the walk ends with what it found, as the guard has always
/// treated a listing it could not take.
///
/// Nothing is deleted here, so a crash part way through recovery
/// leaves the directory exactly as this pass found it and the next
/// open reaches the same verdict.
pub(super) fn suspect_tables(env: &dyn Env, sst_dir: &Path, keyring: Option<&Keyring>) -> Suspects {
    let mut suspects = Suspects::default();
    let mut ignored_empty = 0usize;
    let mut ignored_no_data = 0usize;
    let walk = match env.read_dir(sst_dir) {
        Ok(walk) => walk,
        Err(e) => {
            unlisted(sst_dir, &e);
            return suspects;
        }
    };
    for entry in walk {
        let path = match entry {
            Ok(entry) => entry.path,
            Err(e) => {
                unlisted(sst_dir, &e);
                break;
            }
        };
        if path.extension().and_then(|ext| ext.to_str()) != Some("sst") {
            continue;
        }
        let len = match env.metadata(&path) {
            Ok(meta) => Some(meta.len),
            Err(e) => {
                suspects.add(SuspectTable {
                    path,
                    len: None,
                    reason: format!("unreadable: {e}"),
                });
                continue;
            }
        };
        if len == Some(0) {
            tracing::debug!(
                path = %path.display(),
                "ignoring a zero-length orphan SSTable left by a crash inside a flush"
            );
            ignored_empty += 1;
            continue;
        }
        match table_carries_data(env, &path, keyring) {
            Ok(true) => suspects.add(SuspectTable {
                path,
                len,
                reason: "carries data".to_string(),
            }),
            Ok(false) => {
                tracing::debug!(
                    path = %path.display(),
                    "ignoring an orphan SSTable whose footer records no entry and no range tombstone"
                );
                ignored_no_data += 1;
            }
            Err(e) => suspects.add(SuspectTable {
                path,
                len,
                reason: format!("unreadable footer: {e}"),
            }),
        }
    }
    if ignored_empty + ignored_no_data > 0 {
        tracing::warn!(
            dir = %sst_dir.display(),
            zero_length = ignored_empty,
            no_entries = ignored_no_data,
            "ignoring orphan SSTables that hold nothing, left by a crash inside a flush"
        );
    }
    suspects
}

fn unlisted(sst_dir: &Path, error: &std::io::Error) {
    tracing::warn!(
        dir = %sst_dir.display(),
        error = %error,
        "could not list the SSTable directory while checking for discarded tables"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suspect(name: &str) -> SuspectTable {
        SuspectTable {
            path: PathBuf::from(name),
            len: Some(1),
            reason: "carries data".to_string(),
        }
    }

    #[test]
    fn keeps_the_first_by_path_and_counts_the_rest() {
        let mut suspects = Suspects::default();
        // Every arrival order a walk could give, through a reversed run.
        for i in (0..100).rev() {
            suspects.add(suspect(&format!("{i:06}.sst")));
            assert!(suspects.named.len() <= SUSPECTS_NAMED);
        }
        assert_eq!(suspects.count(), 100);
        let named: Vec<_> = suspects.named.iter().map(|s| s.path.clone()).collect();
        let want: Vec<_> = (0..SUSPECTS_NAMED)
            .map(|i| PathBuf::from(format!("{i:06}.sst")))
            .collect();
        assert_eq!(named, want);
        let message = suspects.describe();
        assert!(message.starts_with("000000.sst (1 bytes, carries data), "));
        assert!(
            message.ends_with(", and 92 more not named here"),
            "{message}"
        );
    }

    #[test]
    fn names_every_suspect_when_they_fit() {
        let mut suspects = Suspects::default();
        for name in ["b.sst", "a.sst", "c.sst"] {
            suspects.add(suspect(name));
        }
        assert_eq!(suspects.count(), 3);
        assert_eq!(
            suspects.describe(),
            "a.sst (1 bytes, carries data), b.sst (1 bytes, carries data), \
             c.sst (1 bytes, carries data)"
        );
    }

    #[test]
    fn a_later_smaller_path_displaces_the_largest_named() {
        let mut suspects = Suspects::default();
        for i in 10..10 + SUSPECTS_NAMED {
            suspects.add(suspect(&format!("{i:06}.sst")));
        }
        suspects.add(suspect("000001.sst"));
        assert_eq!(suspects.named.len(), SUSPECTS_NAMED);
        assert_eq!(suspects.named[0].path, PathBuf::from("000001.sst"));
        assert!(
            !suspects
                .named
                .iter()
                .any(|s| s.path == Path::new(&format!("{:06}.sst", 10 + SUSPECTS_NAMED - 1)))
        );
        assert_eq!(suspects.count(), SUSPECTS_NAMED + 1);
    }
}
