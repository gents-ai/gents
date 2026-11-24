//! The collection a delete or a purge ends with: every shared table no
//! backup lists is removed, a bounded batch at a time.
//!
//! A sweep of `shared/`. The walk of `shared/` hands its tables out a batch
//! of [`SWEEP`] at a time; for each batch the collection walks `meta/` and
//! reads every backup's listing, one at a time, striking each candidate a
//! listing names; what no listing named is removed, and the walk of
//! `shared/` goes on. So it holds one batch of candidates and one listing,
//! never every listed id and never the pool, however many backups and
//! tables the repository keeps, and still removes every table no backup
//! lists: those a deleted backup held, whether its metadata read or not,
//! and those a backup cut short copied and never listed.
//!
//! Every backup counts, sealed or not, since each keeps its listing
//! readable without a key. A listing that cannot be read, or a walk of
//! `meta/` that breaks part way, stops the collection before it removes
//! anything more: leaving that listing out could remove a table the backup
//! still needs. The rules are `ListedRestores`, `CollectionBounded` and
//! `SweepRemovesEveryUnlisted` in `proofs/tla/BackupSeal.tla`, and
//! `sweep_eq` in `proofs/lean/Regolith/BackupSeal.lean`.

use std::collections::HashSet;
use std::io;

use super::{BackupEngine, parse_backup_id, parse_shared_filename, shared_filename};
use crate::{Error, Result};

/// Candidate tables one batch of the sweep holds: 16 KiB of content ids.
pub(super) const SWEEP: usize = 1024;

impl BackupEngine {
    /// Remove every shared table no backup in the repository lists, a batch
    /// of [`SWEEP`] at a time (module docs). Only names the engine gives a
    /// shared table are considered; anything else in `shared/` stays.
    ///
    /// Starts with a sync of `meta/`, and removes nothing unless it succeeds:
    /// a backup's metadata removed by this call, or by an earlier one whose
    /// sync failed and that a retry finds already gone, is then durable
    /// before any table it listed goes. Without it a power cut could bring
    /// the metadata back and keep the removals, leaving a backup that lists
    /// tables the pool no longer has (`BackupSeal.tla`, `SyncOnlyRemoved`).
    pub(super) fn collect_shared(&self) -> Result<()> {
        self.env.sync_dir(&self.meta_dir).map_err(Error::from)?;
        let mut walk = self.env.read_dir(&self.shared_dir).map_err(Error::from)?;
        let mut removed_any = false;
        let mut first = true;
        loop {
            // The next batch: tables the walk hands out, until it holds
            // SWEEP of them or the walk ends. The table that fills it is in
            // it, so none is lost where one batch ends and the next begins.
            let mut batch = HashSet::with_capacity(SWEEP);
            let mut ended = true;
            for entry in walk.by_ref() {
                let entry = entry.map_err(Error::from)?;
                let Some(hash) = parse_shared_filename(&entry.file_name()) else {
                    continue;
                };
                if entry.is_dir {
                    continue;
                }
                batch.insert(hash);
                if batch.len() == SWEEP {
                    ended = false;
                    break;
                }
            }
            // An empty batch after the first has nothing to strike; the
            // first is swept even empty, so a listing that cannot be read
            // is reported whatever `shared/` holds.
            if first || !batch.is_empty() {
                removed_any |= self.sweep(batch, removed_any)?;
            }
            first = false;
            if ended {
                return Ok(());
            }
        }
    }

    /// Strike from `unlisted` every table a backup's listing names, reading
    /// the listings one at a time as the walk of `meta/` reaches them, then
    /// remove what is left. Whether it removed any table.
    fn sweep(&self, mut unlisted: HashSet<u128>, removed_before: bool) -> Result<bool> {
        for entry in self.env.read_dir(&self.meta_dir).map_err(Error::from)? {
            let entry = entry.map_err(Error::from)?;
            let Some(id) = parse_backup_id(&entry.file_name()) else {
                continue;
            };
            let listing = self.read_listing(id).map_err(|e| {
                let what = if removed_before {
                    "no more shared files were removed"
                } else {
                    "no shared file was removed"
                };
                Error::from(io::Error::new(
                    e.kind(),
                    format!("backup {id} cannot be read, so {what}: {e}"),
                ))
            })?;
            for (hash, _) in &listing.objects {
                unlisted.remove(hash);
            }
        }
        let mut removed = false;
        for hash in unlisted {
            match self
                .env
                .remove_file(&self.shared_dir.join(shared_filename(hash)))
            {
                Ok(()) => removed = true,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(Error::from(e)),
            }
        }
        // A removal a power cut undoes brings back a table no backup lists,
        // which the next collection removes; one sync covers the batch.
        if removed {
            self.env.sync_dir(&self.shared_dir).map_err(Error::from)?;
        }
        Ok(removed)
    }
}
