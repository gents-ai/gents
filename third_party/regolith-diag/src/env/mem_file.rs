//! A file held in memory that readers read without any lock: the contents
//! behind [`super::MemEnv`] and the OPFS mirror.
//!
//! # Shape
//!
//! A file is one *extent* at a time, published through a [`kovan::Atom`]:
//! a chain of buffers (`extent.rs`) and one state word holding the
//! published length and two flags.
//!
//! - **Bytes below the published length never change.** A reader loads the
//!   extent, loads the length (Acquire), and copies from below it, with no
//!   lock and no retry, while writers work above it or on a new extent.
//! - **An append in place.** A write at the end sets `WRITER` with one CAS,
//!   links one more buffer to the chain if the room runs out, copies its
//!   bytes above the length, and publishes the new length with a second CAS
//!   that clears the flag. The bytes it copies are above every published
//!   length, so no reader can be reading them, and a growing file never
//!   copies the bytes it already holds.
//! - **Everything else copies.** An overwrite below the length, a shrink, or
//!   an append that meets another append in place builds a new extent from
//!   the published bytes, *freezes* the old one (a CAS that sets `FROZEN`,
//!   after which no append can publish there), and swaps the new one in with
//!   a CAS on the `Atom`. An append whose publishing CAS meets `FROZEN` lost
//!   the race and writes again, on the new extent. The copy reads only below
//!   the length it froze, never the bytes a concurrent appender may be
//!   writing.
//!
//! - **An append at one length only.** The positional log of `MemEnv`
//!   (`mem_log.rs`) appends a record only while the file is exactly as long
//!   as the record's offset ([`MemFile::append_at`]). It is the append
//!   above with that one condition, read from the same state word the
//!   append then moves, so it lands at that length or not at all. A second
//!   thread appending the same record while the first is still copying
//!   does not wait for it: it copies, as any write that meets `WRITER`
//!   does, and the first finds the file already past its offset.
//!
//! Every write is lock-free (a failed CAS means another write landed), and a
//! read is wait-free. A read sees every write that finished before it began:
//! a finished append published its length on the extent every later load
//! returns, or on one a later copy copied from; a finished copy is the extent
//! every later load returns. `proofs/tla/EnvFiles.tla` models this protocol,
//! and the shared file cursor positional reads replace.

#![allow(unsafe_code)]

use std::io;

use kovan::{Atom, AtomGuard};

use crate::portability::Ordering;
use extent::{Extent, MIN_CAPACITY};

mod extent;

/// The published length.
const LEN: u64 = (1 << 62) - 1;
/// An append in place holds the room above the length.
const WRITER: u64 = 1 << 62;
/// A copy replaced or is replacing this extent: nothing more publishes here.
const FROZEN: u64 = 1 << 63;

/// The capacity from which an extent a copy replaced is reclaimed promptly
/// (`engine::reclaim`): such a copy rewrites a whole file, so the
/// reclaimer's fixed cost is small beside it.
const RECLAIM_PROMPTLY: usize = 64 * 1024;

/// What a write costs the memory it is charged to, if anything: the OPFS
/// mirror bounds its resident bytes, `MemEnv` charges nothing.
pub(crate) trait Charge {
    /// Reserve `bytes` more before a write makes them visible. An error
    /// refuses the write, which then changes nothing.
    fn reserve(&self, bytes: u64) -> io::Result<()>;
    /// Give back `bytes`: a reservation whose attempt lost a race or
    /// failed, or a file that shrank.
    fn release(&self, bytes: u64);
}

/// Charges nothing.
pub(crate) struct Free;

impl Charge for Free {
    fn reserve(&self, _bytes: u64) -> io::Result<()> {
        Ok(())
    }

    fn release(&self, _bytes: u64) {}
}

/// What a write does to the file.
enum Change<'a> {
    /// Append these slices at the end, wherever the end is when it lands.
    Append(&'a [&'a [u8]]),
    /// Append these slices only while the file is exactly this long.
    AppendAt(u64, &'a [&'a [u8]]),
    /// Write these bytes at `at`, extending the file with zeros to reach it.
    /// Only the OPFS mirror writes at an offset.
    #[cfg(any(test, all(target_arch = "wasm32", target_os = "unknown")))]
    At(u64, &'a [u8]),
    /// Make the file this long, cutting it or extending it with zeros.
    SetLen(u64),
}

impl Change<'_> {
    /// Where the write starts and the length after it, for a file now `len`.
    fn bounds(&self, len: u64) -> io::Result<(u64, u64)> {
        let (at, end) = match self {
            Change::Append(slices) | Change::AppendAt(_, slices) => {
                let n: u64 = slices.iter().map(|s| s.len() as u64).sum();
                (len, len.checked_add(n))
            }
            #[cfg(any(test, all(target_arch = "wasm32", target_os = "unknown")))]
            Change::At(at, buf) => (*at, at.checked_add(buf.len() as u64).map(|e| e.max(len))),
            Change::SetLen(new) => (len.min(*new), Some(*new)),
        };
        match end {
            Some(end) if end <= LEN => Ok((at, end)),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the write would make the file too large to address",
            )),
        }
    }

    /// Write this change's bytes into `extent`, starting at `at`.
    ///
    /// # Safety
    ///
    /// The caller owns `[at, to)` of `extent` exclusively (see
    /// [`Extent::copy_in`]).
    unsafe fn fill(&self, extent: &Extent, at: usize) {
        match self {
            Change::Append(slices) | Change::AppendAt(_, slices) => {
                let mut at = at;
                for slice in slices.iter() {
                    // SAFETY: forwarded from the caller.
                    unsafe { extent.copy_in(at, slice) };
                    at += slice.len();
                }
            }
            // SAFETY: forwarded from the caller.
            #[cfg(any(test, all(target_arch = "wasm32", target_os = "unknown")))]
            Change::At(_, buf) => unsafe { extent.copy_in(at, buf) },
            // What an extension exposes is zeroed where it is copied or
            // extended in place, before the fill.
            Change::SetLen(_) => {}
        }
    }
}

/// What [`MemFile::append_at`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppendAt {
    /// The file was as long as the offset: the bytes are appended, and the
    /// file is this long now.
    Appended(u64),
    /// The file is longer than the offset: what starts there is written.
    Past,
    /// The file is shorter than the offset: a gap lies below it.
    Short,
}

/// A file held in memory, read without any lock. See the module docs.
pub(crate) struct MemFile {
    current: Atom<Extent>,
}

fn to_usize(n: u64) -> io::Result<usize> {
    usize::try_from(n).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "offset is too large to address",
        )
    })
}

impl MemFile {
    /// An empty file.
    pub(crate) fn new() -> Self {
        Self {
            current: Atom::new(Extent::empty()),
        }
    }

    /// A file holding `bytes`.
    #[cfg(any(test, all(target_arch = "wasm32", target_os = "unknown")))]
    pub(crate) fn from_vec(bytes: Vec<u8>) -> Self {
        Self {
            current: Atom::new(Extent::holding(bytes)),
        }
    }

    /// The file's length now.
    pub(crate) fn len(&self) -> u64 {
        self.current.load().state.load(Ordering::Acquire) & LEN
    }

    /// Copy the bytes at `offset` into `buf`, as many as the file holds
    /// there, and return how many. Never blocks.
    #[cfg(any(test, all(target_arch = "wasm32", target_os = "unknown")))]
    pub(crate) fn read_at(&self, offset: u64, buf: &mut [u8]) -> usize {
        let extent = self.current.load();
        let len = extent.state.load(Ordering::Acquire) & LEN;
        if offset >= len {
            return 0;
        }
        // Below `len`, which fits the buffer, so fits `usize`.
        let n = buf.len().min((len - offset) as usize);
        // SAFETY: `offset + n <= len`, loaded above with Acquire.
        unsafe { extent.copy_out(offset as usize, &mut buf[..n]) };
        n
    }

    /// Fill `buf` from `offset`, or fail with `UnexpectedEof` when the file
    /// is shorter.
    pub(crate) fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let end = offset
            .checked_add(buf.len() as u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "read range overflows"))?;
        let extent = self.current.load();
        let len = extent.state.load(Ordering::Acquire) & LEN;
        if end > len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "failed to fill whole buffer",
            ));
        }
        // SAFETY: `end <= len`, loaded above with Acquire.
        unsafe { extent.copy_out(to_usize(offset)?, buf) };
        Ok(())
    }

    /// The whole file, as one copy.
    #[cfg(any(test, all(target_arch = "wasm32", target_os = "unknown")))]
    pub(crate) fn to_vec(&self) -> Vec<u8> {
        let extent = self.current.load();
        let len = (extent.state.load(Ordering::Acquire) & LEN) as usize;
        let mut out = vec![0u8; len];
        // SAFETY: `len` loaded above with Acquire.
        unsafe { extent.copy_out(0, &mut out) };
        out
    }

    /// Append `slices` at the end. Returns the length before and after.
    pub(crate) fn append(&self, slices: &[&[u8]], charge: &dyn Charge) -> io::Result<(u64, u64)> {
        self.change(&Change::Append(slices), charge)
    }

    /// Append `slices` only if the file is exactly `at` bytes long now, and
    /// say which side of `at` its length was on otherwise. Never waits: see
    /// the module docs.
    pub(crate) fn append_at(
        &self,
        at: u64,
        slices: &[&[u8]],
        charge: &dyn Charge,
    ) -> io::Result<AppendAt> {
        let (before, after) = self.change(&Change::AppendAt(at, slices), charge)?;
        Ok(match before.cmp(&at) {
            std::cmp::Ordering::Equal => AppendAt::Appended(after),
            std::cmp::Ordering::Greater => AppendAt::Past,
            std::cmp::Ordering::Less => AppendAt::Short,
        })
    }

    /// Write `buf` at `at`. Returns the length before and after.
    #[cfg(any(test, all(target_arch = "wasm32", target_os = "unknown")))]
    pub(crate) fn write_at(
        &self,
        at: u64,
        buf: &[u8],
        charge: &dyn Charge,
    ) -> io::Result<(u64, u64)> {
        self.change(&Change::At(at, buf), charge)
    }

    /// Cut or zero-extend the file to `len`. Returns the length before and
    /// after.
    pub(crate) fn set_len(&self, len: u64, charge: &dyn Charge) -> io::Result<(u64, u64)> {
        self.change(&Change::SetLen(len), charge)
    }

    fn change(&self, change: &Change<'_>, charge: &dyn Charge) -> io::Result<(u64, u64)> {
        loop {
            let extent = self.current.load();
            let state = settled(&extent);
            let len = state & LEN;
            if let Change::AppendAt(only_at, _) = change
                && *only_at != len
            {
                // Not the length the append is for: nothing changes, and
                // the caller reads which side of it the file is on.
                return Ok((len, len));
            }
            let (at, end) = change.bounds(len)?;
            let grow = end.saturating_sub(len);
            charge.reserve(grow)?;
            let in_place = state & (WRITER | FROZEN) == 0 && at == len && end >= len;
            let attempt = if in_place {
                self.in_place(&extent, state, change, len, end)
            } else {
                self.copy(&extent, state, change, at, len, end)
            };
            match attempt {
                Ok(true) => {
                    if end < len {
                        charge.release(len - end);
                    }
                    // A copy retired the extent it replaced. A large one is
                    // handed to the reclaimer now, once this thread no
                    // longer reaches it, rather than when enough other
                    // retirements fill this thread's batch.
                    let replaced = (!in_place).then(|| extent.capacity());
                    drop(extent);
                    if replaced.is_some_and(|capacity| capacity >= RECLAIM_PROMPTLY) {
                        crate::engine::reclaim::submit();
                        #[cfg(test)]
                        SUBMITTED.with(|n| n.set(n.get() + 1));
                    }
                    return Ok((len, end));
                }
                // Nothing landed, whether the attempt lost a race or failed:
                // the reservation goes back either way.
                Ok(false) => charge.release(grow),
                Err(err) => {
                    charge.release(grow);
                    return Err(err);
                }
            }
        }
    }

    /// Append at the end of `extent`, linking more room when it runs out.
    /// `false` when another write got there first.
    fn in_place(
        &self,
        extent: &AtomGuard<'_, Extent>,
        state: u64,
        change: &Change<'_>,
        len: u64,
        end: u64,
    ) -> io::Result<bool> {
        let (from, to) = (to_usize(len)?, to_usize(end)?);
        if extent
            .state
            .compare_exchange(state, state | WRITER, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return Ok(false);
        }
        // SAFETY: `WRITER` is held.
        if let Err(err) = unsafe { extent.reserve(to) } {
            // Give `WRITER` back. A copy that froze the extent meanwhile has
            // taken the file elsewhere, and the word stays as it froze it.
            let _ = extent.state.compare_exchange(
                state | WRITER,
                state,
                Ordering::Release,
                Ordering::Relaxed,
            );
            return Err(err);
        }
        if let Change::SetLen(_) = change {
            // Bytes above the length may hold a lost attempt's bytes.
            // SAFETY: `WRITER` is held and the range is above the length.
            unsafe { extent.zero(from, to - from) };
        }
        // SAFETY: `WRITER` is held, and `[from, to)` is above every length
        // published on this extent.
        unsafe { change.fill(extent, from) };
        #[cfg(test)]
        if let Some(hook) = BETWEEN_WRITE_AND_PUBLISH.with(|slot| slot.borrow_mut().take()) {
            hook();
        }
        // Release: the bytes, and the room they were written to, are visible
        // to every reader that loads this length. Fails only when a copy
        // froze the extent meanwhile.
        Ok(extent
            .state
            .compare_exchange(state | WRITER, end, Ordering::Release, Ordering::Relaxed)
            .is_ok())
    }

    /// Build a new extent holding the file after `change`, freeze `extent`,
    /// and swap the new one in. `Ok(false)` when another write got there
    /// first.
    fn copy(
        &self,
        extent: &AtomGuard<'_, Extent>,
        state: u64,
        change: &Change<'_>,
        at: u64,
        len: u64,
        end: u64,
    ) -> io::Result<bool> {
        #[cfg(test)]
        COPIED.with(|n| n.set(n.get() + 1));
        // Grows by doubling only when the bytes need more room; a copy for an
        // overwrite or a lost race keeps the capacity it had, in one segment.
        let need = to_usize(end)?;
        let capacity = if need <= extent.capacity() {
            extent.capacity()
        } else {
            need.max(extent.capacity().saturating_mul(2))
                .max(MIN_CAPACITY)
        };
        let fresh = Extent::with_capacity(capacity)?;
        let kept = len.min(end) as usize;
        // The bytes between the old end and the change's own bytes: an
        // extension's whole range, a write past the end's gap. They read as
        // zeros, and `fresh` holds nothing there yet.
        let gap_end = match change {
            Change::SetLen(_) => need,
            _ => at as usize,
        };
        // SAFETY: `kept <= len`, loaded with Acquire by the caller; `fresh`
        // is not published, so this thread owns all of it.
        unsafe {
            fresh.copy_prefix(extent, kept);
            if gap_end > kept {
                fresh.zero(kept, gap_end - kept);
            }
            change.fill(&fresh, at as usize);
        }
        fresh.state.store(end, Ordering::Relaxed);
        #[cfg(test)]
        if let Some(hook) = BETWEEN_BUILD_AND_FREEZE.with(|slot| slot.borrow_mut().take()) {
            hook();
        }
        // Freeze: from here no append publishes on the old extent, so the
        // bytes copied above are all it will ever hold. A frozen extent
        // stays frozen; only the swap below decides.
        if state & FROZEN == 0
            && extent
                .state
                .compare_exchange(state, state | FROZEN, Ordering::AcqRel, Ordering::Relaxed)
                .is_err()
        {
            return Ok(false);
        }
        Ok(self.current.compare_and_swap(extent, fresh).is_ok())
    }
}

/// How many times an append looks again at an extent another append is
/// writing in place before it copies instead. Only an optimization: a copy
/// is always correct, and costs a whole file.
const SETTLE_SPINS: u32 = 64;

/// The extent's state, looked at again a few times while another append
/// holds `WRITER`, so a short race ends in place rather than in a copy.
/// Never waits beyond the bound: what it returns may still hold `WRITER`.
fn settled(extent: &Extent) -> u64 {
    let mut state = extent.state.load(Ordering::Acquire);
    for _ in 0..SETTLE_SPINS {
        if state & WRITER == 0 || state & FROZEN != 0 {
            break;
        }
        std::hint::spin_loop();
        state = extent.state.load(Ordering::Acquire);
    }
    state
}

impl Default for MemFile {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
thread_local! {
    /// Replaced extents this thread handed to the reclaimer.
    static SUBMITTED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Copies this thread built.
    static COPIED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many replaced extents this thread has handed to the reclaimer.
#[cfg(test)]
fn submitted() -> usize {
    SUBMITTED.with(std::cell::Cell::get)
}

/// How many copies this thread has built.
#[cfg(test)]
fn copied() -> usize {
    COPIED.with(std::cell::Cell::get)
}

#[cfg(test)]
thread_local! {
    /// Runs once, on this thread, inside the next copy, after it built its
    /// new extent and before it freezes the old one: the window where
    /// another write may publish or freeze.
    static BETWEEN_BUILD_AND_FREEZE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `hook` inside this thread's next copy, between its build and its
/// freeze.
#[cfg(test)]
fn between_build_and_freeze(hook: impl FnOnce() + 'static) {
    BETWEEN_BUILD_AND_FREEZE.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}

#[cfg(test)]
thread_local! {
    /// Runs once, on this thread, inside the next append in place, after it
    /// wrote its bytes and before its publishing CAS: the window where a
    /// copy may freeze the extent.
    static BETWEEN_WRITE_AND_PUBLISH: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `hook` inside this thread's next append in place, between its write
/// and its publish.
#[cfg(test)]
pub(super) fn between_write_and_publish(hook: impl FnOnce() + 'static) {
    BETWEEN_WRITE_AND_PUBLISH.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}

#[cfg(test)]
mod tests;
