//! A flush publishes its table and retires its memtable as one view: no
//! view holds both, so no read sees the memtable's entries twice.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use crate::{Db, MergeOperator, Options};

/// Concatenates operands onto the base, so an operand applied twice shows.
struct Append;

impl MergeOperator for Append {
    fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut out = base.map(<[u8]>::to_vec).unwrap_or_default();
        for operand in operands {
            out.extend_from_slice(operand);
        }
        Some(out)
    }

    fn partial_merge(&self, _key: &[u8], left: &[u8], right: &[u8]) -> Option<Vec<u8>> {
        let mut out = left.to_vec();
        out.extend_from_slice(right);
        Some(out)
    }

    fn name(&self) -> &'static str {
        "append"
    }
}

/// Reads made right after the flush published the version holding its
/// table see each merge operand once, by point lookup and by scan. With the
/// memtable retired in a later publication, the view in between held the
/// memtable and its table at once, and both reads returned "abab".
#[test]
fn reads_right_after_a_flush_installs_its_table_count_each_operand_once() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(
        Db::open(
            dir.path(),
            Options::default()
                .max_background_compactions(0)
                .merge_operator(Some(Arc::new(Append))),
        )
        .unwrap(),
    );
    db.merge(b"k", b"a").unwrap();
    db.merge(b"k", b"b").unwrap();
    let ran = Arc::new(AtomicBool::new(false));
    {
        let (db, ran) = (Arc::clone(&db), Arc::clone(&ran));
        super::after_next_flush_install(move || {
            assert_eq!(
                db.get(b"k").unwrap(),
                Some(b"ab".to_vec()),
                "a point read in the flush's window applied an operand twice"
            );
            assert_eq!(
                db.scan(None, None).unwrap(),
                vec![(b"k".to_vec(), b"ab".to_vec())],
                "a scan in the flush's window applied an operand twice"
            );
            ran.store(true, Ordering::Release);
        });
    }
    db.flush().unwrap();
    assert!(ran.load(Ordering::Acquire), "setup: the flush ran its hook");
    assert_eq!(db.get(b"k").unwrap(), Some(b"ab".to_vec()));
}

/// The same on a compaction worker, in the round that gives one turn each
/// to a job, the flushes and a pass, with the flush exclusion a mutex the
/// worker waits for by parking: the flush it runs after a job still
/// installs its table and retires its memtable in one view.
#[test]
fn a_worker_flush_between_a_job_and_a_pass_counts_each_operand_once() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(
        Db::open(
            dir.path(),
            Options::default()
                .write_buffer_size(4 * 1024)
                .merge_operator(Some(Arc::new(Append))),
        )
        .unwrap(),
    );
    db.merge(b"k", b"a").unwrap();
    db.merge(b"k", b"b").unwrap();

    // The hook is per thread, so a job arms it on the worker.
    let (armed, when_armed) = mpsc::channel();
    let (seen, saw) = mpsc::channel();
    {
        let reader = Arc::clone(&db);
        db.engine().compaction_scheduler().run_on_worker(move || {
            super::after_next_flush_install(move || {
                let _ = seen.send((reader.get(b"k"), reader.scan(None, None)));
            });
            armed.send(()).unwrap();
        });
    }
    when_armed
        .recv_timeout(Duration::from_secs(60))
        .expect("the worker never ran the job");

    // Fill the buffer: the writer seals it and the worker flushes it.
    for i in 0..64 {
        crate::through_stalls(|| db.put(format!("fill{i:03}").as_bytes(), &[7u8; 512])).unwrap();
    }
    let (point, scan) = saw
        .recv_timeout(Duration::from_secs(60))
        .expect("the worker never flushed the sealed memtable");
    assert_eq!(
        point.unwrap(),
        Some(b"ab".to_vec()),
        "a point read in the worker's flush window applied an operand twice"
    );
    let scanned = scan.unwrap();
    let merged = scanned.iter().find(|(key, _)| key == b"k");
    assert_eq!(
        merged.map(|(_, value)| value.as_slice()),
        Some(b"ab".as_slice()),
        "a scan in the worker's flush window applied an operand twice"
    );
}
