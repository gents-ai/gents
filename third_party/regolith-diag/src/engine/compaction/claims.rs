//! Which tables the compaction passes in flight are working on, claimed with
//! atomics so that passes sharing the compaction gate never pick overlapping
//! inputs and never wait for one another.
//!
//! # Claims
//!
//! Every live table carries a claim flag ([`LiveSst::claimed`]). A pass picks
//! its inputs from the current version, skipping claimed tables, then claims
//! them in ascending id order, one compare-and-swap each. When one is taken
//! already it gives back what it took and reports `Contended`; ascending
//! order means that of two passes reaching for the same tables, the one that
//! takes the lowest shared id goes on, so they never both give up for the
//! same pair.
//!
//! # Still live
//!
//! A table leaves the version only by a pass that holds its claim, or by an
//! exclusive user of the compaction gate, which no pass overlaps. A holder
//! lets go of a claim only after its install is published. So after taking
//! its claims a pass reads the current version again ([`all_live`]): a table
//! it finds there stays there until its own claim is released, and one it
//! does not find was compacted away between its pick and its claim, which it
//! reports as `Contended` rather than compacting it a second time.
//!
//! # The whole pool
//!
//! FIFO and universal compaction merge or drop across the whole L0 pool, so
//! a pass of theirs claims the pool instead of tables. One word counts the
//! tables claimed one by one and has a bit for the pool: the pool is claimed
//! only while no table is, and no table while the pool is.

use std::sync::Arc;

use crate::portability::{AtomicUsize, Ordering};

use super::LiveSst;

/// The bit of [`Claims`]'s word a pool claim sets. The bits below it count
/// the tables claimed one by one.
const POOL: usize = 1 << (usize::BITS - 1);

/// What every pass of one engine shares: the claims on tables, whose turn
/// the next pass is, and the census.
#[derive(Default)]
pub(crate) struct Claims {
    word: AtomicUsize,
    /// Whether size-triggered work or a deletion pass goes first
    /// (`deletion.rs`).
    pub(crate) turn: super::deletion::Turn,
    /// The count of deletions in tables recorded without one
    /// (`census.rs`).
    pub(crate) census: super::census::Census,
}

/// What one pass holds, given back on drop, whatever way the pass ends.
#[must_use = "dropping the claim releases it at once"]
pub(crate) struct Claim<'a> {
    claims: &'a Claims,
    tables: Vec<Arc<LiveSst>>,
    pool: bool,
}

impl Claims {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Claim `tables`, or `None` when another pass holds one of them or the
    /// pool. The caller then checks they are [`all_live`].
    pub(crate) fn tables(&self, mut tables: Vec<Arc<LiveSst>>) -> Option<Claim<'_>> {
        let count = tables.len();
        // Counted in before any flag is set, so a pool claim that finds the
        // count at zero knows no table is about to be claimed past it.
        let mut word = self.word.load(Ordering::Acquire);
        loop {
            if word & POOL != 0 {
                return None;
            }
            match self.word.compare_exchange_weak(
                word,
                word + count,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(actual) => word = actual,
            }
        }
        tables.sort_unstable_by_key(|table| table.meta.file_id);
        let taken = tables
            .iter()
            .take_while(|table| {
                table
                    .claimed
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            })
            .count();
        if taken < count {
            for table in &tables[..taken] {
                table.claimed.store(false, Ordering::Release);
            }
            self.word.fetch_sub(count, Ordering::AcqRel);
            return None;
        }
        Some(Claim {
            claims: self,
            tables,
            pool: false,
        })
    }

    /// Claim the whole L0 pool, or `None` while another pass holds it or any
    /// table.
    pub(crate) fn pool(&self) -> Option<Claim<'_>> {
        self.word
            .compare_exchange(0, POOL, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Claim {
                claims: self,
                tables: Vec::new(),
                pool: true,
            })
    }

    /// No pass holds anything.
    #[cfg(test)]
    pub(crate) fn is_idle(&self) -> bool {
        self.word.load(Ordering::Acquire) == 0
    }
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        if self.pool {
            self.claims.word.fetch_and(!POOL, Ordering::AcqRel);
            return;
        }
        for table in &self.tables {
            table.claimed.store(false, Ordering::Release);
        }
        self.claims
            .word
            .fetch_sub(self.tables.len(), Ordering::AcqRel);
    }
}

/// Whether every table of `claimed` is still in `level`, by identity: read
/// after the claims are taken (see the module docs).
pub(crate) fn all_live(level: &[Arc<LiveSst>], claimed: &[Arc<LiveSst>]) -> bool {
    claimed
        .iter()
        .all(|table| level.iter().any(|live| Arc::ptr_eq(live, table)))
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use crate::engine::sstable::{SsTableMeta, SsTableReader, SsTableWriter};
    use crate::options::CompressionType;
    use std::sync::atomic::AtomicUsize as StdAtomicUsize;
    use tempfile::TempDir;

    fn table(dir: &TempDir, file_id: u64) -> Arc<LiveSst> {
        let path = dir.path().join(format!("{file_id}.sst"));
        let mut writer =
            SsTableWriter::new(&path, 4096, 10, CompressionType::None, None, false, 4096).unwrap();
        writer
            .add(
                &crate::engine::internal_key::encode_internal_key(
                    b"k",
                    1,
                    crate::engine::internal_key::VALUE_TYPE_VALUE,
                ),
                b"v",
            )
            .unwrap();
        writer.finish().unwrap().unwrap();
        LiveSst::new(
            SsTableMeta {
                file_id,
                smallest_key: b"k".to_vec(),
                largest_key: b"k".to_vec(),
                file_size: 1,
                num_entries: 1,
                global_seq: None,
                deletions: None,
            },
            Arc::new(SsTableReader::open(&path, file_id).unwrap()),
            1,
        )
    }

    #[test]
    fn a_claimed_table_is_not_claimed_twice_and_comes_back_on_drop() {
        let dir = TempDir::new().unwrap();
        let (a, b, c) = (table(&dir, 1), table(&dir, 2), table(&dir, 3));
        let claims = Claims::new();
        let first = claims
            .tables(vec![Arc::clone(&c), Arc::clone(&b)])
            .expect("free");
        // Claimed in id order: `a` is taken first, then `c` refuses.
        assert!(
            claims
                .tables(vec![Arc::clone(&c), Arc::clone(&a)])
                .is_none(),
            "c is held"
        );
        assert!(
            !a.claimed.load(Ordering::Acquire),
            "a refused claim gives back what it took"
        );
        let other = claims.tables(vec![Arc::clone(&a)]).expect("a is free");
        drop(first);
        assert!(!b.claimed.load(Ordering::Acquire) && !c.claimed.load(Ordering::Acquire));
        drop(other);
        assert!(claims.is_idle());
    }

    #[test]
    fn the_pool_and_single_tables_exclude_each_other() {
        let dir = TempDir::new().unwrap();
        let a = table(&dir, 1);
        let claims = Claims::new();
        let pool = claims.pool().expect("idle");
        assert!(claims.pool().is_none(), "one pool claim at a time");
        assert!(claims.tables(vec![Arc::clone(&a)]).is_none());
        assert!(!a.claimed.load(Ordering::Acquire));
        drop(pool);
        let one = claims.tables(vec![Arc::clone(&a)]).expect("free");
        assert!(claims.pool().is_none(), "a table is held");
        drop(one);
        assert!(claims.pool().is_some());
    }

    #[test]
    fn liveness_is_by_identity() {
        let dir = TempDir::new().unwrap();
        let (a, b) = (table(&dir, 1), table(&dir, 2));
        assert!(all_live(
            &[Arc::clone(&a), Arc::clone(&b)],
            &[Arc::clone(&a)]
        ));
        // The same id in a fresh table is another table.
        let a_again = table(&dir, 1);
        assert!(!all_live(&[a_again, b], &[a]));
    }

    /// Passes racing for overlapping sets: no table is ever held by two at
    /// once, and the claims all come back.
    #[test]
    fn racing_passes_never_share_a_table() {
        let dir = TempDir::new().unwrap();
        let tables: Vec<_> = (1..=6).map(|id| table(&dir, id)).collect();
        let holders: Vec<StdAtomicUsize> =
            (0..tables.len()).map(|_| StdAtomicUsize::new(0)).collect();
        let claims = Claims::new();
        std::thread::scope(|scope| {
            for t in 0..4usize {
                let (tables, holders, claims) = (&tables, &holders, &claims);
                scope.spawn(move || {
                    // Fewer rounds under miri, which runs each a hundred times
                    // slower.
                    for round in 0..if cfg!(miri) { 40 } else { 2_000usize } {
                        let start = (t + round) % tables.len();
                        let set: Vec<usize> = (0..3)
                            .map(|i| (start + i * (t + 1)) % tables.len())
                            .collect();
                        let mut picked: Vec<usize> = set.clone();
                        picked.sort_unstable();
                        picked.dedup();
                        let chosen = picked.iter().map(|&i| Arc::clone(&tables[i])).collect();
                        if let Some(claim) = claims.tables(chosen) {
                            for &i in &picked {
                                assert_eq!(
                                    holders[i].fetch_add(1, std::sync::atomic::Ordering::SeqCst),
                                    0,
                                    "table {i} is held by two passes"
                                );
                            }
                            std::hint::spin_loop();
                            for &i in &picked {
                                holders[i].fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                            }
                            drop(claim);
                        }
                    }
                });
            }
        });
        assert!(claims.is_idle());
        assert!(tables.iter().all(|t| !t.claimed.load(Ordering::Acquire)));
    }
}
