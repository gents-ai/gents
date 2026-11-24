//! An acknowledgement record is never cut.
//!
//! The same failure as the journal's (`cut_record.rs`), in the file where a
//! crash child records what it was told: written as lines of their own
//! length, a kill could leave a line cut at a page boundary, with another
//! thread's whole line run onto it. `common/fault/acks.rs` writes records of
//! one length that divides a page and refuses a file that is not whole
//! records; these tests pin both halves.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::common::fault::{self, ACK_BYTES, Acks, ChildSpec, CrashRun, Phase, Trigger, read_acks};
use tempfile::TempDir;

const PAGE: usize = 4096;

/// Why reading `path` as records of `N` numbers was refused, if it was.
fn refusal<const N: usize>(path: &Path) -> Option<String> {
    std::panic::catch_unwind(|| read_acks(path).of::<N>())
        .err()
        .map(|panic| {
            panic
                .downcast_ref::<String>()
                .cloned()
                .unwrap_or_else(|| "a panic with no message".to_string())
        })
}

/// The bytes the writer appends for `records`, in a file of their own in
/// `dir`.
fn written<const N: usize>(dir: &Path, records: &[[u64; N]]) -> Vec<u8> {
    let path = dir.join(format!("written-{}", records.len() * 7 + N));
    Acks::open(&path).ack_all(records.iter().copied());
    std::fs::read(&path).unwrap()
}

/// What a child appends is read back record for record, each one record of
/// [`ACK_BYTES`] bytes, and a file never written reads as no record.
///
/// Catches: a writer that writes a record at its own length, which puts a
/// record across a page boundary, where a kill can cut it.
#[test]
fn acknowledgements_are_read_back_as_written_each_a_whole_record() {
    assert_eq!(PAGE % ACK_BYTES, 0);
    let tmp = TempDir::new().unwrap();
    let pairs = tmp.path().join("pairs");
    let mut acks = Acks::open(&pairs);
    let written: Vec<[u64; 2]> = (0..300).map(|i| [i % 4, i / 4 * 1_000_003]).collect();
    for record in &written[..100] {
        acks.ack(*record);
    }
    acks.ack_all(written[100..].iter().copied());
    assert_eq!(read_acks(&pairs).of::<2>(), written);
    let bytes = std::fs::read(&pairs).unwrap();
    assert_eq!(bytes.len(), written.len() * ACK_BYTES);
    assert!(
        bytes
            .chunks(ACK_BYTES)
            .all(|record| record.iter().position(|b| *b == b'\n') == Some(ACK_BYTES - 1)),
        "a record's only line end is its last byte"
    );

    let values = tmp.path().join("values");
    Acks::open(&values).ack_all([[u64::MAX], [0], [7]]);
    assert_eq!(read_acks(&values).of::<1>(), [[u64::MAX], [0], [7]]);

    assert!(read_acks(&tmp.path().join("never written")).is_empty());
}

/// A record its numbers do not fit in is refused by the writer, which then
/// writes nothing.
#[test]
fn an_acknowledgement_that_does_not_fit_a_record_is_refused_by_the_writer() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("acks");
    let mut acks = Acks::open(&path);
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        acks.ack([u64::MAX, u64::MAX]);
    }));
    assert!(refused.is_err());
    assert!(read_acks(&path).is_empty());
}

/// Whatever is not whole records of the shape asked for is refused, and the
/// refusal names the file: a record cut at the end wherever the cut fell, a
/// cut record with another thread's whole record run onto it, lines of their
/// own length as the suites wrote them before, and a record of another
/// shape.
///
/// Catches: a reader that drops what it cannot read, or reads the front of
/// a cut record as a record (a cut number reads as a smaller one).
#[test]
fn an_acknowledgement_file_that_is_not_whole_records_is_refused_naming_the_file() {
    let tmp = TempDir::new().unwrap();
    let whole = written(tmp.path(), &(0..10).map(|i| [i % 4, i]).collect::<Vec<_>>());
    let next = written(tmp.path(), &[[3, 12]]);
    let one = written(tmp.path(), &[[5]]);
    let mut damaged: Vec<(String, Vec<u8>)> = Vec::new();
    for keep in [1, 2, 4, ACK_BYTES - 1] {
        damaged.push((
            format!("a record cut after {keep} bytes, at the end"),
            [&whole[..], &next[..keep]].concat(),
        ));
        damaged.push((
            format!("a record cut after {keep} bytes, with a whole one run onto it"),
            [&whole[..], &next[..keep], &next[..]].concat(),
        ));
    }
    damaged.push((
        "lines of their own length".into(),
        b"0 0\n1 1\n2 2\n3 3\n0 4\n1 5\n2 6\n3 7\n".to_vec(),
    ));
    damaged.push((
        "a record of one number among records of two".into(),
        [&whole[..ACK_BYTES], &one[..]].concat(),
    ));
    for (n, (what, bytes)) in damaged.iter().enumerate() {
        let path = tmp.path().join(format!("damaged-{n}"));
        std::fs::write(&path, bytes).unwrap();
        let refused = refusal::<2>(&path);
        assert!(
            refused
                .as_deref()
                .is_some_and(|why| why.contains(&path.display().to_string())),
            "{what}: {refused:?}"
        );
    }
}

const STORM: &str = "ack-storm";
/// Threads acknowledging while the kill lands.
const STORM_THREADS: u64 = 4;
/// Children the storm test kills.
const STORM_KILLS: u64 = 500;

/// The length of the acknowledgement file at which the storm with `seed`
/// sends its kill: a page boundary or up to 15 records short of one. The
/// other threads go on appending while the kill is delivered, so the seeds
/// spread it over what they write then.
fn storm_target(seed: u64) -> u64 {
    PAGE as u64 * (1 + seed % 6) - (seed / 6 % 16) * 32
}

/// Threads that do nothing but acknowledge, each its own count from 0; the
/// first of them also kills the process once the file reaches
/// [`storm_target`].
fn storm(spec: &ChildSpec) {
    let target = storm_target(spec.seed);
    let started = Arc::new(AtomicUsize::new(0));
    let threads: Vec<_> = (0..STORM_THREADS)
        .map(|t| {
            let mut acks = Acks::open(&spec.ack_path);
            let (started, path) = (Arc::clone(&started), spec.ack_path.clone());
            std::thread::spawn(move || {
                started.fetch_add(1, Ordering::AcqRel);
                while started.load(Ordering::Acquire) < STORM_THREADS as usize {
                    std::thread::yield_now();
                }
                let mut i = 0;
                loop {
                    acks.ack([t, i]);
                    i += 1;
                    if t == 0 && std::fs::metadata(&path).map_or(0, |m| m.len()) >= target {
                        fault::kill_self();
                    }
                }
            })
        })
        .collect();
    for thread in threads {
        let _ = thread.join();
    }
}

#[test]
fn storm_child() {
    fault::child_entrypoint(storm);
}

/// Children whose threads acknowledge as fast as they can are killed as the
/// file nears a page boundary, and every file they leave is whole records,
/// each thread's a count from 0 with no gap.
///
/// The same storm with the suites' former lines left a line cut at a page
/// boundary in 23 of 6,000 kills (tmpfs, Linux 6.18), 4 of them with
/// another thread's line run onto the cut one.
///
/// Catches: any way of writing an acknowledgement that a kill can cut.
#[test]
fn a_kill_while_threads_acknowledge_cuts_no_record() {
    let tmp = TempDir::new().unwrap();
    for seed in 0..STORM_KILLS {
        let db = tmp.path().join(format!("db-{seed}"));
        let out = CrashRun::new(ChildSpec::new(Phase::Custom(STORM.into()), &db).seed(seed))
            .entry_test("cut_ack::storm_child")
            .trigger(Trigger::Workload)
            .record_io(false)
            .run();
        out.assert_killed();
        let mut next: BTreeMap<u64, u64> = BTreeMap::new();
        for [t, i] in out.acks.of::<2>() {
            let expected = next.entry(t).or_default();
            assert_eq!(
                (t < STORM_THREADS, i),
                (true, *expected),
                "seed {seed}: thread {t}'s acknowledgements are not a count from 0"
            );
            *expected += 1;
        }
        std::fs::remove_dir_all(&db).unwrap();
    }
}
