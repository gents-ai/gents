//! Warn soon after open when the database's filesystem is close to full.
//!
//! A full disk does not announce itself: flushes and compactions start
//! failing with ENOSPC, and a filesystem out of inodes fails the same way
//! with bytes to spare. Checking once after open turns that into a warning
//! an operator sees before the first failed write. The check is background
//! work (D15): never on the open path, so a slow filesystem (a stalled
//! network mount, say) never delays the open, and never on a thread of its
//! own. The compaction worker runs it; a database with no worker owes it to
//! its first write, which runs it on its thread's queue, or inline with no
//! queue (`background_step.rs`). It only ever logs: nothing is gated on the
//! answer.

use std::path::PathBuf;
use std::sync::Arc;

use super::io::job::JobBody;
use crate::env::{DiskSpace, Env};

/// Below this many free bytes the disk counts as low, however large it is.
const LOW_BYTES: u64 = 1 << 30;
/// Below this many free inodes the filesystem counts as low.
const LOW_INODES: u64 = 10_000;
/// Below this share of the total (as a divisor: 1/20 = 5%) either counts
/// as low.
const LOW_SHARE_DIVISOR: u64 = 20;

/// What is low on the filesystem, if anything.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Shortage {
    pub(crate) bytes: bool,
    pub(crate) inodes: bool,
}

pub(crate) fn assess(space: &DiskSpace) -> Shortage {
    let low = |available: u64, total: u64, floor: u64| {
        available < floor || (total > 0 && available < total / LOW_SHARE_DIVISOR)
    };
    Shortage {
        bytes: low(space.available_bytes, space.total_bytes, LOW_BYTES),
        // A filesystem that reports no inode total allocates them on
        // demand (btrfs, ZFS); only a fixed table can run out.
        inodes: space.total_inodes > 0
            && low(space.available_inodes, space.total_inodes, LOW_INODES),
    }
}

/// The disk check as a job for the compaction worker.
pub(crate) struct DiskCheck {
    env: Arc<dyn Env>,
    dir: PathBuf,
}

impl DiskCheck {
    pub(crate) fn new(env: Arc<dyn Env>, dir: PathBuf) -> Self {
        Self { env, dir }
    }
}

impl JobBody for DiskCheck {
    fn run(self: Box<Self>) {
        check(&*self.env, &self.dir);
    }

    /// Skipped when the worker stopped first: the check is advisory.
    fn release(self: Box<Self>) {}
}

/// Check the filesystem holding `dir` and log a warning if it is low on
/// space or inodes. One `statvfs`-style call through `env`.
pub(crate) fn check(env: &dyn Env, dir: &std::path::Path) {
    let space = match env.disk_space(dir) {
        Ok(Some(space)) => space,
        Ok(None) => return,
        Err(e) => {
            tracing::debug!(dir = %dir.display(), error = %e, "could not read disk space");
            return;
        }
    };
    let shortage = assess(&space);
    if shortage.bytes || shortage.inodes {
        tracing::warn!(
            dir = %dir.display(),
            available_bytes = space.available_bytes,
            total_bytes = space.total_bytes,
            available_inodes = space.available_inodes,
            total_inodes = space.total_inodes,
            low_on_space = shortage.bytes,
            low_on_inodes = shortage.inodes,
            "the database's filesystem is nearly full; flushes and compactions will fail \
             with ENOSPC when it fills"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    #[test]
    fn plenty_of_both_is_fine() {
        let space = DiskSpace::new(500 * GIB, 1000 * GIB, 5_000_000, 10_000_000);
        assert_eq!(assess(&space), Shortage::default());
    }

    #[test]
    fn under_the_byte_floor_is_low_even_on_a_small_disk() {
        let space = DiskSpace::new(GIB / 2, 4 * GIB, 900_000, 1_000_000);
        assert_eq!(
            assess(&space),
            Shortage {
                bytes: true,
                inodes: false
            }
        );
    }

    #[test]
    fn under_five_percent_is_low_on_a_large_disk() {
        let space = DiskSpace::new(40 * GIB, 1000 * GIB, 900_000, 1_000_000);
        assert!(assess(&space).bytes);
    }

    #[test]
    fn running_out_of_inodes_is_low_with_bytes_to_spare() {
        let space = DiskSpace::new(500 * GIB, 1000 * GIB, 5_000, 10_000_000);
        assert_eq!(
            assess(&space),
            Shortage {
                bytes: false,
                inodes: true
            }
        );
    }

    #[test]
    fn a_filesystem_without_an_inode_table_is_never_low_on_inodes() {
        let space = DiskSpace::new(500 * GIB, 1000 * GIB, 0, 0);
        assert!(!assess(&space).inodes);
    }

    #[cfg(unix)]
    #[test]
    fn the_std_env_reports_the_filesystem_it_is_on() {
        let dir = tempfile::tempdir().unwrap();
        let space = crate::env::std_env()
            .disk_space(dir.path())
            .unwrap()
            .expect("unix reports disk space");
        assert!(space.total_bytes > 0);
        assert!(space.available_bytes <= space.total_bytes);
    }
}
