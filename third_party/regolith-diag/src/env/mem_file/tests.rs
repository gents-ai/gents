//! The in-memory file: its writes against a sequential model, and reads
//! racing appends, overwrites and shrinks on many threads.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64 as StdAtomicU64, Ordering as StdOrdering};

use proptest::prelude::*;

use super::*;

fn file_with(bytes: &[u8]) -> MemFile {
    let file = MemFile::new();
    file.append(&[bytes], &Free).unwrap();
    file
}

#[test]
fn appends_overwrites_and_lengths_round_trip() {
    let file = MemFile::new();
    assert_eq!(file.append(&[b"0123", b"4567"], &Free).unwrap(), (0, 8));
    assert_eq!(file.write_at(2, b"ab", &Free).unwrap(), (8, 8));
    assert_eq!(file.to_vec(), b"01ab4567");
    assert_eq!(file.write_at(10, b"z", &Free).unwrap(), (8, 11));
    assert_eq!(file.to_vec(), b"01ab4567\0\0z");
    assert_eq!(file.set_len(3, &Free).unwrap(), (11, 3));
    assert_eq!(file.to_vec(), b"01a");
    assert_eq!(file.set_len(5, &Free).unwrap(), (3, 5));
    assert_eq!(file.to_vec(), b"01a\0\0");
    let mut buf = [0u8; 4];
    assert_eq!(file.read_at(3, &mut buf), 2);
    assert_eq!(
        file.read_exact_at(2, &mut buf).unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
    assert_eq!(MemFile::from_vec(b"xyz".to_vec()).to_vec(), b"xyz");
}

#[test]
fn an_extension_after_a_cut_reads_zeros_not_old_bytes() {
    let file = file_with(b"secret");
    file.set_len(0, &Free).unwrap();
    file.set_len(6, &Free).unwrap();
    assert_eq!(file.to_vec(), [0u8; 6]);
}

/// A copy builds its extent without zeroing the room it grows into, so an
/// extension or a write past the end that copies zeroes what it exposes.
#[test]
fn an_extension_that_copies_reads_zeros_where_it_exposes_bytes() {
    let file = file_with(b"abc");
    let capacity = file.current.load().capacity();
    let past = capacity as u64 * 4;
    assert_eq!(file.set_len(past, &Free).unwrap(), (3, past));
    let mut expected = b"abc".to_vec();
    expected.resize(past as usize, 0);
    assert_eq!(file.to_vec(), expected);

    let far = past * 4;
    assert_eq!(file.write_at(far, b"z", &Free).unwrap(), (past, far + 1));
    expected.resize(far as usize, 0);
    expected.push(b'z');
    assert_eq!(file.to_vec(), expected);
}

/// A file grows in place: an append past its room links one more segment,
/// as large as the room before it, and copies none of the bytes the file
/// already holds. Reads across the segments see the bytes as written.
#[test]
fn a_growing_file_links_room_and_copies_nothing() {
    let file = MemFile::new();
    let before = copied();
    let mut model = Vec::new();
    for i in 0..(1usize << 20) / 1000 {
        let record = vec![(i % 251) as u8; 1000];
        file.append(&[&record], &Free).unwrap();
        model.extend_from_slice(&record);
    }
    assert_eq!(copied(), before, "an append in place copies nothing");
    let extent = file.current.load();
    assert!(extent.capacity() < 2 * model.len() + 4096);
    assert!(extent.segment_count() <= 22, "the chain doubles its room");
    drop(extent);
    assert_eq!(file.to_vec(), model);
    let mut buf = vec![0u8; 70_000];
    file.read_exact_at(12_345, &mut buf).unwrap();
    assert_eq!(buf, model[12_345..12_345 + buf.len()]);
}

/// An overwrite of a large file copies it, and hands the extent it replaced
/// to the reclaimer at once rather than leaving it in this thread's batch
/// until enough other retirements fill it.
#[test]
fn a_copy_of_a_large_file_hands_the_old_extent_to_the_reclaimer() {
    let file = MemFile::new();
    file.append(&[&vec![1u8; 4 * RECLAIM_PROMPTLY]], &Free)
        .unwrap();
    let before = submitted();
    file.write_at(0, b"x", &Free).unwrap();
    assert_eq!(submitted() - before, 1);
    let small = file_with(b"small");
    let before = submitted();
    small.write_at(0, b"S", &Free).unwrap();
    assert_eq!(submitted(), before, "a small extent waits for its batch");
    assert_eq!(small.to_vec(), b"Small");
}

/// An append publishes in place while a copy is between its build and its
/// freeze. The copy's freeze CAS expects the word it built from, so it fails,
/// and the copy writes again from the longer file: the append survives.
#[test]
fn a_copy_racing_an_append_in_place_starts_again() {
    let file = Arc::new(file_with(b"a"));
    let racing = Arc::clone(&file);
    between_build_and_freeze(move || {
        racing.append(&[b"b"], &Free).unwrap();
    });
    file.write_at(0, b"x", &Free).unwrap();
    assert_eq!(file.to_vec(), b"xb");
}

/// The copy's extent is frozen by another copy after this one built. The
/// length may have grown before that freeze, so this copy must not trust
/// "frozen now" and swap the copy it built: it writes again. Planting the
/// stale check (`MC_EnvFiles_Red_StaleFrozen`) loses the append.
#[test]
fn a_copy_built_before_another_freeze_starts_again() {
    let file = Arc::new(file_with(b"a"));
    let racing = Arc::clone(&file);
    between_build_and_freeze(move || {
        racing.append(&[b"b"], &Free).unwrap();
        // Another copy froze the extent and has not swapped yet.
        racing
            .current
            .load()
            .state
            .fetch_or(FROZEN, Ordering::AcqRel);
    });
    file.write_at(0, b"x", &Free).unwrap();
    assert_eq!(file.to_vec(), b"xb");
}

/// An append in place wrote its bytes, and before it publishes a copy (an
/// overwrite) freezes the extent and swaps a new one in. The append's
/// publishing CAS fails on the frozen word, and it appends again on the new
/// extent: it lands once, after the overwrite. A plain store in place of the
/// CAS would publish on the dead extent and lose it.
#[test]
fn an_append_frozen_out_appends_again_once() {
    let file = Arc::new(file_with(b"a"));
    let racing = Arc::clone(&file);
    between_write_and_publish(move || {
        racing.write_at(0, b"x", &Free).unwrap();
    });
    file.append(&[b"b"], &Free).unwrap();
    assert_eq!(file.to_vec(), b"xb");
}

/// A charge that refuses past a bound, counting what it holds.
struct Bounded {
    held: StdAtomicU64,
    max: u64,
}

impl Charge for Bounded {
    fn reserve(&self, bytes: u64) -> io::Result<()> {
        // A compare-and-swap loop, which every toolchain from the MSRV to
        // the newest stable spells the same way.
        let mut held = self.held.load(StdOrdering::Acquire);
        loop {
            if held + bytes > self.max {
                return Err(io::Error::other("over the bound"));
            }
            match self.held.compare_exchange_weak(
                held,
                held + bytes,
                StdOrdering::AcqRel,
                StdOrdering::Acquire,
            ) {
                Ok(_) => return Ok(()),
                Err(now) => held = now,
            }
        }
    }

    fn release(&self, bytes: u64) {
        self.held.fetch_sub(bytes, StdOrdering::AcqRel);
    }
}

#[test]
fn a_refused_charge_changes_nothing_and_a_cut_gives_bytes_back() {
    let charge = Bounded {
        held: StdAtomicU64::new(0),
        max: 6,
    };
    let file = MemFile::new();
    file.append(&[b"abcd"], &charge).unwrap();
    assert!(file.append(&[b"xyz"], &charge).is_err());
    assert_eq!(file.to_vec(), b"abcd");
    file.set_len(1, &charge).unwrap();
    assert_eq!(charge.held.load(StdOrdering::SeqCst), 1);
    file.append(&[b"xyz"], &charge).unwrap();
    assert_eq!(charge.held.load(StdOrdering::SeqCst), 4);
}

/// A write whose charge was reserved and whose attempt then fails, here
/// because no buffer that large can be had, gives the reservation back: a
/// failed write leaks no charge, as a refused one does not.
#[test]
fn a_write_that_fails_after_its_charge_was_reserved_gives_it_back() {
    let charge = Bounded {
        held: StdAtomicU64::new(0),
        max: u64::MAX,
    };
    let file = MemFile::new();
    file.append(&[b"abcd"], &charge).unwrap();
    let err = file.set_len(1 << 61, &charge).unwrap_err();
    assert_eq!(
        err.kind(),
        io::ErrorKind::OutOfMemory,
        "setup: the copy cannot get its buffer"
    );
    assert_eq!(
        charge.held.load(StdOrdering::SeqCst),
        4,
        "the failed write kept its charge"
    );
    assert_eq!(file.to_vec(), b"abcd");
    file.append(&[b"ef"], &charge).unwrap();
    assert_eq!(charge.held.load(StdOrdering::SeqCst), 6);
}

/// One appender writes numbered records; readers on other threads read the
/// whole file again and again. Every read is a prefix of the records, and
/// holds at least every record whose append returned before the read began.
#[test]
fn a_read_sees_every_append_that_finished_before_it() {
    const RECORDS: u64 = 4_000;
    let file = Arc::new(MemFile::new());
    let finished = Arc::new(StdAtomicU64::new(0));
    let done = Arc::new(AtomicBool::new(false));
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let (file, finished, done) =
                (Arc::clone(&file), Arc::clone(&finished), Arc::clone(&done));
            std::thread::spawn(move || {
                let mut last = 0;
                while !done.load(StdOrdering::Acquire) {
                    let before = finished.load(StdOrdering::Acquire);
                    let bytes = file.to_vec();
                    assert_eq!(bytes.len() % 8, 0, "a torn record");
                    let records = bytes.len() as u64 / 8;
                    assert!(
                        records >= before,
                        "{records} records, {before} finished before the read"
                    );
                    assert!(records >= last, "the file shrank under a reader");
                    for (i, record) in bytes.chunks(8).enumerate() {
                        assert_eq!(record, (i as u64).to_le_bytes(), "record {i}");
                    }
                    last = records;
                }
            })
        })
        .collect();
    for i in 0..RECORDS {
        file.append(&[&i.to_le_bytes()], &Free).unwrap();
        finished.store(i + 1, StdOrdering::Release);
    }
    done.store(true, StdOrdering::Release);
    for reader in readers {
        reader.join().unwrap();
    }
    assert_eq!(file.len(), RECORDS * 8);
}

/// Several appenders on one file, racing in place and through copies:
/// every record lands exactly once and whole.
#[test]
fn concurrent_appenders_lose_and_tear_nothing() {
    const WRITERS: u64 = 6;
    const EACH: u64 = 1_500;
    let file = Arc::new(MemFile::new());
    let writers: Vec<_> = (0..WRITERS)
        .map(|w| {
            let file = Arc::clone(&file);
            std::thread::spawn(move || {
                for i in 0..EACH {
                    let id = w * EACH + i;
                    // Two slices: a record split across the vectored write
                    // must still land contiguously.
                    let bytes = id.to_le_bytes();
                    file.append(&[&bytes[..3], &bytes[3..]], &Free).unwrap();
                    if i % 97 == 0 {
                        // A copy racing the appends.
                        file.write_at(0, &file.to_vec()[..8], &Free).unwrap();
                    }
                }
            })
        })
        .collect();
    for writer in writers {
        writer.join().unwrap();
    }
    let bytes = file.to_vec();
    let ids: BTreeSet<u64> = bytes
        .chunks(8)
        .map(|chunk| u64::from_le_bytes(chunk.try_into().unwrap()))
        .collect();
    assert_eq!(bytes.len() as u64, WRITERS * EACH * 8);
    assert_eq!(ids, (0..WRITERS * EACH).collect());
}

/// One step of the sequential model.
#[derive(Clone, Debug)]
enum Op {
    Append(Vec<u8>),
    WriteAt(u16, Vec<u8>),
    SetLen(u16),
    Read(u16, u8),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        prop::collection::vec(any::<u8>(), 0..300).prop_map(Op::Append),
        (0u16..600, prop::collection::vec(any::<u8>(), 0..64))
            .prop_map(|(at, b)| Op::WriteAt(at, b)),
        (0u16..600).prop_map(Op::SetLen),
        (0u16..600, any::<u8>()).prop_map(|(at, n)| Op::Read(at, n)),
    ]
}

proptest! {
    /// Every operation agrees with a `Vec<u8>` doing the same.
    #[test]
    fn the_file_follows_a_vec(ops in prop::collection::vec(op(), 1..60)) {
        let file = MemFile::new();
        let mut model: Vec<u8> = Vec::new();
        for op in ops {
            match op {
                Op::Append(bytes) => {
                    let before = model.len() as u64;
                    model.extend_from_slice(&bytes);
                    prop_assert_eq!(file.append(&[&bytes], &Free).unwrap(), (before, model.len() as u64));
                }
                Op::WriteAt(at, bytes) => {
                    let at = at as usize;
                    let before = model.len() as u64;
                    if model.len() < at + bytes.len() {
                        model.resize(at + bytes.len(), 0);
                    }
                    model[at..at + bytes.len()].copy_from_slice(&bytes);
                    prop_assert_eq!(file.write_at(at as u64, &bytes, &Free).unwrap(), (before, model.len() as u64));
                }
                Op::SetLen(len) => {
                    let before = model.len() as u64;
                    model.resize(len as usize, 0);
                    prop_assert_eq!(file.set_len(len as u64, &Free).unwrap(), (before, len as u64));
                }
                Op::Read(at, n) => {
                    let mut buf = vec![0u8; n as usize];
                    let got = file.read_at(at as u64, &mut buf);
                    let want = model.get(at as usize..).map_or(0, |rest| rest.len().min(n as usize));
                    prop_assert_eq!(got, want);
                    let at = at as usize;
                    prop_assert_eq!(&buf[..got], model.get(at..at + got).unwrap_or(&[]));
                }
            }
            prop_assert_eq!(file.len(), model.len() as u64);
            prop_assert_eq!(file.to_vec(), model.clone());
        }
    }
}

#[test]
fn an_append_at_a_length_lands_only_at_that_length() {
    let file = file_with(b"0123");
    assert_eq!(file.append_at(2, &[b"xx"], &Free).unwrap(), AppendAt::Past);
    assert_eq!(file.append_at(6, &[b"xx"], &Free).unwrap(), AppendAt::Short);
    assert_eq!(file.to_vec(), b"0123", "neither changed the file");
    assert_eq!(
        file.append_at(4, &[b"45", b"67"], &Free).unwrap(),
        AppendAt::Appended(8)
    );
    assert_eq!(file.to_vec(), b"01234567");
    // The same record again: written already.
    assert_eq!(
        file.append_at(4, &[b"4567"], &Free).unwrap(),
        AppendAt::Past
    );
    assert_eq!(file.to_vec(), b"01234567");
}

/// A second append of one record meets the first between its copy and its
/// publish. It does not wait: it copies and lands, and the first, whose
/// publish then fails, finds the file past its offset. The record is there
/// once.
#[test]
fn two_appends_at_one_length_land_once_and_neither_waits() {
    let file = Arc::new(file_with(b"stamp"));
    let racing = Arc::clone(&file);
    between_write_and_publish(move || {
        assert_eq!(
            racing.append_at(5, &[b"record"], &Free).unwrap(),
            AppendAt::Appended(11)
        );
    });
    assert_eq!(
        file.append_at(5, &[b"record"], &Free).unwrap(),
        AppendAt::Past
    );
    assert_eq!(file.to_vec(), b"stamprecord");
}
