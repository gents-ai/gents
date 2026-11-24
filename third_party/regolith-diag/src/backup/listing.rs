//! Listing the backups in `meta/`, in id order, a bounded page at a time.
//!
//! Id order needs a sort, and a sort of the whole directory would hold
//! every backup id at once. Instead each page is one walk of `meta/` that
//! keeps the [`PAGE`] smallest ids above the last one listed, in a heap of
//! that size. Listing a repository of `n` backups takes `n / PAGE + 1`
//! walks and holds one page of ids and one backup's listing at any time,
//! whatever the repository holds.
//!
//! Every backup is listed, readable or not: one whose metadata cannot be
//! read is an [`UnreadableBackup`] item, never left out, so the listing's
//! length is the number of backups the repository holds.

use std::collections::BinaryHeap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{BackupId, BackupInfo, UnreadableBackup, parse_backup_id, read_listing, summarize};
use crate::env::Env;
use crate::{Error, Result};

/// Backup ids one page holds: 8 KiB of ids.
pub(super) const PAGE: usize = 1024;

/// The `limit` smallest backup ids above `after` in `meta_dir`, ascending,
/// from one walk of the directory that holds at most `limit` ids. Only the
/// names the engine writes count ([`parse_backup_id`]): a stray `7.backup`
/// beside `000007.backup` is not a second backup 7.
pub(super) fn page_after(
    env: &dyn Env,
    meta_dir: &Path,
    after: Option<BackupId>,
    limit: usize,
) -> io::Result<Vec<BackupId>> {
    // A max-heap of the smallest ids seen so far: a smaller id displaces
    // the largest one held once the heap is full.
    let mut smallest = BinaryHeap::with_capacity(limit);
    for entry in env.read_dir(meta_dir)? {
        let Some(id) = parse_backup_id(&entry?.file_name()) else {
            continue;
        };
        if after.is_some_and(|after| id <= after) {
            continue;
        }
        if smallest.len() < limit {
            smallest.push(id);
        } else if let Some(mut largest) = smallest.peek_mut()
            && id < *largest
        {
            *largest = id;
        }
    }
    Ok(smallest.into_sorted_vec())
}

/// Every backup id in `meta/`, ascending, a page at a time: what the
/// listing and a purge walk.
///
/// The next page is walked only when the last one is used up. An error
/// walking `meta/` is yielded as an `Err` item and ends the walk: the ids
/// after it are not known.
pub(super) struct Ids {
    env: Arc<dyn Env>,
    meta_dir: PathBuf,
    /// The page being walked, the ids not yet yielded.
    page: std::vec::IntoIter<BackupId>,
    /// The last id taken from a page; the next page starts above it.
    after: Option<BackupId>,
    /// No page is left to walk.
    done: bool,
}

impl Ids {
    pub(super) fn new(env: Arc<dyn Env>, meta_dir: PathBuf) -> Self {
        Self {
            env,
            meta_dir,
            page: Vec::new().into_iter(),
            after: None,
            done: false,
        }
    }
}

impl Iterator for Ids {
    type Item = io::Result<BackupId>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(id) = self.page.next() {
                self.after = Some(id);
                return Some(Ok(id));
            }
            if self.done {
                return None;
            }
            match page_after(&*self.env, &self.meta_dir, self.after, PAGE) {
                Ok(page) => {
                    // A short page is the last one.
                    self.done = page.len() < PAGE;
                    self.page = page.into_iter();
                }
                Err(e) => {
                    self.done = true;
                    return Some(Err(e));
                }
            }
        }
    }
}

/// The backups in a repository, in id order (creation order), from
/// [`super::BackupEngine::list_backups`].
///
/// Streamed a page at a time: the next page is walked only when the last
/// one is used up, so a listing holds one page of ids and one backup's
/// listing whatever the repository holds. It owns what it needs, so the
/// engine can delete the backups it lists while it lists them.
///
/// Each item is `Ok` with the backup's entry: its summary when its metadata
/// reads, an [`UnreadableBackup`] with its id and the reason when it does
/// not. An `Err` item says a walk of `meta/` failed, and ends the listing:
/// the backups after it are not known, and the error says so rather than
/// end the listing as if there were none.
pub struct Backups {
    ids: Ids,
}

impl Backups {
    pub(super) fn new(env: Arc<dyn Env>, meta_dir: PathBuf) -> Self {
        Self {
            ids: Ids::new(env, meta_dir),
        }
    }

    /// Backup `id`'s entry: its summary, or why its metadata cannot be read.
    fn entry(&self, id: BackupId) -> std::result::Result<BackupInfo, UnreadableBackup> {
        read_listing(&*self.ids.env, &self.ids.meta_dir, id)
            .and_then(|listing| summarize(id, &listing))
            .map_err(|e| UnreadableBackup {
                id,
                reason: Error::from(e),
            })
    }
}

impl Iterator for Backups {
    type Item = Result<std::result::Result<BackupInfo, UnreadableBackup>>;

    fn next(&mut self) -> Option<Self::Item> {
        Some(match self.ids.next()? {
            Ok(id) => Ok(self.entry(id)),
            Err(e) => Err(Error::from(e)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::backup_filename;
    use super::*;
    use crate::env::MemEnv;

    fn meta_dir(env: &dyn Env, ids: impl IntoIterator<Item = u64>) -> PathBuf {
        let dir = PathBuf::from("/backups/meta");
        env.create_dir_all(&dir).unwrap();
        for id in ids {
            env.write(&dir.join(backup_filename(BackupId(id))), b"")
                .unwrap();
        }
        dir
    }

    fn raw(ids: Vec<BackupId>) -> Vec<u64> {
        ids.into_iter().map(|id| id.0).collect()
    }

    #[test]
    fn pages_cover_every_id_once_in_order() {
        let env = MemEnv::new();
        // Gaps, as deletes leave them, created in no particular order.
        let ids: Vec<u64> = (1..=50).rev().filter(|id| id % 7 != 0).collect();
        let dir = meta_dir(&env, ids.iter().copied());
        let mut listed = Vec::new();
        let mut after = None;
        loop {
            let page = page_after(&env, &dir, after, 4).unwrap();
            assert!(page.len() <= 4, "a page held {} ids", page.len());
            assert!(page.windows(2).all(|w| w[0] < w[1]), "{page:?}");
            let Some(&last) = page.last() else {
                break;
            };
            listed.extend(raw(page));
            after = Some(last);
        }
        let mut want = ids;
        want.sort_unstable();
        assert_eq!(listed, want);
    }

    #[test]
    fn a_page_skips_stray_names_and_other_files() {
        let env = MemEnv::new();
        let dir = meta_dir(&env, [3, 1]);
        env.write(&dir.join("1.backup"), b"").unwrap();
        env.write(&dir.join("000002.tmp"), b"").unwrap();
        env.write(&dir.join("notes"), b"").unwrap();
        assert_eq!(raw(page_after(&env, &dir, None, PAGE).unwrap()), [1, 3]);
        assert_eq!(
            raw(page_after(&env, &dir, Some(BackupId(1)), PAGE).unwrap()),
            [3]
        );
        assert!(
            page_after(&env, &dir, Some(BackupId(3)), PAGE)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_walk_error_is_yielded_and_ends_the_listing() {
        let env: Arc<dyn Env> = Arc::new(MemEnv::new());
        let mut backups = Backups::new(env, PathBuf::from("/missing/meta"));
        let first = backups.next().expect("an item");
        assert!(
            matches!(&first, Err(Error::Io(e)) if e.kind() == io::ErrorKind::NotFound),
            "{first:?}"
        );
        assert!(backups.next().is_none());
    }

    #[test]
    fn every_backup_is_listed_past_a_page_readable_or_not() {
        let env: Arc<dyn Env> = Arc::new(MemEnv::new());
        let last = PAGE as u64 + 3;
        let dir = meta_dir(&*env, 1..=last);
        // Empty files: none reads, and each is listed, as unreadable.
        let ids: Vec<u64> = Backups::new(env, dir)
            .map(|item| item.unwrap().unwrap_err().id.0)
            .collect();
        assert_eq!(ids, (1..=last).collect::<Vec<_>>());
    }
}
