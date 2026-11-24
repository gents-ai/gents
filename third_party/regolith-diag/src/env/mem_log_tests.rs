//! The in-memory log: the file holds the contiguous written prefix whatever
//! order the records arrive in and however often, no held write is left
//! behind or appended in place of a later one, and two threads writing one
//! record at once do not wait for each other.

use std::path::Path;
use std::sync::Arc;

use proptest::prelude::*;

use super::*;
use crate::env::mem_file::between_write_and_publish;
use crate::env::{Env, MemEnv, WriteMode};

const LOG: &str = "/db/log";

fn env() -> MemEnv {
    let env = MemEnv::new();
    env.create_dir_all(Path::new("/db")).unwrap();
    env
}

fn open(env: &MemEnv, mode: WriteMode) -> Box<dyn LogFile> {
    env.open_log(Path::new(LOG), mode).unwrap()
}

fn contents(env: &MemEnv) -> Vec<u8> {
    env.read(Path::new(LOG)).unwrap()
}

#[test]
fn a_write_ahead_of_a_gap_reaches_the_file_once_the_gap_is_filled() {
    let env = env();
    let log = open(&env, WriteMode::Truncate);
    log.write_all_at(4, b"5678").unwrap();
    assert_eq!(contents(&env), b"", "a held write is not in the file");
    log.write_all_at(8, b"9abc").unwrap();
    log.write_all_at(0, b"1234").unwrap();
    assert_eq!(contents(&env), b"123456789abc", "ML1 and ML2");
}

#[test]
fn a_repeated_write_changes_nothing() {
    let env = env();
    let log = open(&env, WriteMode::Truncate);
    log.write_all_at(0, b"abcd").unwrap();
    log.write_all_at(0, b"abcd").unwrap();
    log.write_all_at(4, b"ef").unwrap();
    log.write_all_at(4, b"ef").unwrap();
    assert_eq!(contents(&env), b"abcdef");
}

#[test]
fn a_cut_drops_the_tail_and_what_was_held_above_it() {
    let env = env();
    let log = open(&env, WriteMode::Truncate);
    log.write_all_at(0, b"keep").unwrap();
    log.write_all_at(4, b"drop").unwrap();
    log.write_all_at(12, b"held").unwrap();
    log.set_len(4).unwrap();
    assert_eq!(contents(&env), b"keep");
    // The offsets are written again, with other bytes: a new commit. A
    // held write of the old one must not be appended in its place (ML3).
    log.write_all_at(4, b"next").unwrap();
    log.write_all_at(8, b"more").unwrap();
    log.write_all_at(12, b"last").unwrap();
    assert_eq!(contents(&env), b"keepnextmorelast");
}

/// A cut above the file's end leaves the file as it is and keeps what is
/// held below the cut.
#[test]
fn a_cut_above_the_end_keeps_the_file_and_what_is_held_below_it() {
    let env = env();
    let log = open(&env, WriteMode::Truncate);
    log.write_all_at(4, b"held").unwrap();
    log.write_all_at(8, b"gone").unwrap();
    log.set_len(8).unwrap();
    assert_eq!(contents(&env), b"", "nothing was written below the gap");
    log.write_all_at(0, b"head").unwrap();
    assert_eq!(contents(&env), b"headheld");
}

#[test]
fn reopening_for_update_continues_above_what_the_file_holds() {
    let env = env();
    open(&env, WriteMode::Truncate)
        .write_all_at(0, b"stamp")
        .unwrap();
    let log = open(&env, WriteMode::Update);
    assert_eq!(contents(&env), b"stamp");
    log.write_all_at(5, b"record").unwrap();
    assert_eq!(contents(&env), b"stamprecord");
    // A log started over is empty.
    let log = open(&env, WriteMode::Truncate);
    assert_eq!(contents(&env), b"");
    log.write_all_at(0, b"new").unwrap();
    assert_eq!(contents(&env), b"new");
}

/// ML4: a second thread writes a record while the first is between its
/// copy and its publish. It does not wait: it takes the append over, and
/// the first finds the record written. The second write runs inside the
/// first one, on its thread, at the instant another thread's would.
#[test]
fn two_writes_of_one_record_at_once_do_not_wait_for_each_other() {
    let env = env();
    let log: Arc<dyn LogFile> = Arc::from(open(&env, WriteMode::Truncate));
    // The first write gives the file its capacity, so the next lands in
    // place.
    log.write_all_at(0, b"stamp").unwrap();
    let (second, seen) = (Arc::clone(&log), env.clone());
    between_write_and_publish(move || {
        second.write_all_at(5, b"record").unwrap();
        assert_eq!(
            contents(&seen),
            b"stamprecord",
            "the second write returned before its bytes were held"
        );
    });
    log.write_all_at(5, b"record").unwrap();
    assert_eq!(contents(&env), b"stamprecord", "the record is there once");
}

/// ML2, the holder's side: the gap closes after a write found it open and
/// before the write's bytes are in the map. The thread that closed it
/// looked too early, so the holder appends them itself, and what was held
/// above them follows.
#[test]
fn a_gap_that_closes_while_a_write_is_being_held_leaves_nothing_behind() {
    let env = env();
    let log: Arc<dyn LogFile> = Arc::from(open(&env, WriteMode::Truncate));
    log.write_all_at(8, b"cccc").unwrap();
    let below = Arc::clone(&log);
    before_hold(move || below.write_all_at(0, b"1234").unwrap());
    log.write_all_at(4, b"5678").unwrap();
    assert_eq!(contents(&env), b"12345678cccc");
}

/// ML5: a write is about to be held when another thread writes the gap
/// and the same record. Its bytes, put in the map after that, are of a
/// record already in the file: they are taken back out, so nothing that is
/// written stays held for as long as the log lives.
#[test]
fn a_held_write_that_was_written_meanwhile_does_not_stay_held() {
    let data = Arc::new(MemFile::new());
    let log = Arc::new(MemLog::new(Arc::clone(&data)));
    let other = Arc::clone(&log);
    before_hold(move || {
        other.write_all_at(0, b"1234").unwrap();
        other.write_all_at(4, b"same").unwrap();
    });
    log.write_all_at(4, b"same").unwrap();
    assert_eq!(data.to_vec(), b"1234same");
    assert_eq!(log.held_writes(), 0, "a written record stayed held");
}

/// ML3: a commit's write is held, a cut below it discards the commit, and
/// the commit that takes its offset is not answered with the old bytes.
#[test]
fn a_held_write_of_a_discarded_commit_is_not_appended_for_the_next() {
    let env = env();
    let log = open(&env, WriteMode::Truncate);
    log.write_all_at(0, b"12").unwrap();
    log.write_all_at(4, b"old!").unwrap();
    log.set_len(2).unwrap();
    log.write_all_at(2, b"ab").unwrap();
    assert_eq!(
        contents(&env),
        b"12ab",
        "a held write of a discarded commit was appended"
    );
    log.write_all_at(4, b"new!").unwrap();
    assert_eq!(contents(&env), b"12abnew!");
}

/// Many threads write disjoint records of one log at once, each record
/// twice and in an order of its own: the file holds every record, in
/// offset order.
#[test]
fn concurrent_writes_in_any_order_end_up_in_offset_order() {
    const THREADS: usize = 8;
    const RECORDS: usize = if cfg!(miri) { 16 } else { 400 };
    const LEN: usize = 29;
    let env = env();
    let log: Arc<dyn LogFile> = Arc::from(open(&env, WriteMode::Truncate));
    let record = |i: usize| vec![(i % 251) as u8 + 1; LEN];
    std::thread::scope(|scope| {
        for t in 0..THREADS {
            let log = Arc::clone(&log);
            scope.spawn(move || {
                // Its own records back to front, then every record once
                // more from an offset of its own: both out of order and
                // twice.
                for i in (0..RECORDS).rev().filter(|i| i % THREADS == t) {
                    log.write_all_at((i * LEN) as u64, &record(i)).unwrap();
                }
                for step in 0..RECORDS {
                    let i = (step * 7 + t * 13) % RECORDS;
                    log.write_all_at((i * LEN) as u64, &record(i)).unwrap();
                }
            });
        }
    });
    let expected: Vec<u8> = (0..RECORDS).flat_map(record).collect();
    assert!(
        contents(&env) == expected,
        "a record is missing or misplaced"
    );
}

proptest! {
    /// Whatever order the records of a log are written in, and however
    /// often each is written again, the file ends up holding them in
    /// offset order, and holds the contiguous written prefix at every step.
    #[test]
    fn any_write_order_yields_the_records_in_offset_order(
        lens in proptest::collection::vec(1usize..40, 1..24),
        order in proptest::collection::vec(any::<prop::sample::Index>(), 0..64),
    ) {
        let mut records = Vec::new();
        let mut at = 0u64;
        for (i, len) in lens.iter().enumerate() {
            records.push((at, vec![i as u8 + 1; *len]));
            at += *len as u64;
        }
        let env = env();
        let log = open(&env, WriteMode::Truncate);
        let mut written = vec![false; records.len()];
        let prefix = |written: &[bool]| -> Vec<u8> {
            records
                .iter()
                .zip(written)
                .take_while(|(_, written)| **written)
                .flat_map(|((_, bytes), _)| bytes.clone())
                .collect()
        };
        for pick in order {
            let i = pick.index(records.len());
            log.write_all_at(records[i].0, &records[i].1).unwrap();
            written[i] = true;
            prop_assert_eq!(contents(&env), prefix(&written));
        }
        for (i, (at, bytes)) in records.iter().enumerate().rev() {
            log.write_all_at(*at, bytes).unwrap();
            written[i] = true;
        }
        prop_assert_eq!(contents(&env), prefix(&written));
    }
}
