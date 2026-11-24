//! `LogLayout`: `head_key`, `entry_key` and `max_entry_key_len` are asked by
//! `append` and by the commit that numbers the entries, with the positions
//! the log reaches, and the keys they name are the keys written.

use std::sync::{Arc, Mutex};

use regolith::{LogLayout, OptimisticTransactionDb, Options, TxnOptions};

/// A log at `journal-head` and `journal/<position>`, recording each call.
#[derive(Default)]
struct Journal {
    head: Mutex<usize>,
    bound: Mutex<usize>,
    /// Each position asked for, and whether the buffer handed over was
    /// empty.
    entries: Mutex<Vec<(u64, bool)>>,
}

impl LogLayout for Journal {
    fn head_key(&self) -> &[u8] {
        *self.head.lock().unwrap() += 1;
        b"journal-head"
    }

    fn entry_key(&self, position: u64, out: &mut Vec<u8>) {
        self.entries
            .lock()
            .unwrap()
            .push((position, out.is_empty()));
        out.extend_from_slice(b"journal/");
        out.extend_from_slice(&position.to_be_bytes());
    }

    fn max_entry_key_len(&self) -> usize {
        *self.bound.lock().unwrap() += 1;
        b"journal/".len() + 8
    }
}

fn entry_key(position: u64) -> Vec<u8> {
    let mut key = b"journal/".to_vec();
    key.extend_from_slice(&position.to_be_bytes());
    key
}

#[test]
fn every_method_is_asked_and_the_keys_it_names_are_the_keys_written() {
    let db = OptimisticTransactionDb::open("journal", Options::memory()).unwrap();
    let journal = Arc::new(Journal::default());
    let layout: Arc<dyn LogLayout> = journal.clone();

    // An append asks for the longest entry key and the bound it must fit.
    let first = db.begin(&TxnOptions::new());
    first.append(&layout, b"one", None).unwrap();
    first.append(&layout, b"two", Some(b"once/two")).unwrap();
    assert_eq!(
        *journal.entries.lock().unwrap(),
        [(u64::MAX, true), (u64::MAX, true)]
    );
    assert_eq!(*journal.bound.lock().unwrap(), 2);
    assert_eq!(*journal.head.lock().unwrap(), 0);

    // The commit reads the head and asks for the key of each position it
    // gives out, into an empty buffer.
    first.commit().unwrap();
    let second = db.begin(&TxnOptions::new());
    second.append(&layout, b"three", None).unwrap();
    second.commit().unwrap();
    let asked: Vec<(u64, bool)> = journal
        .entries
        .lock()
        .unwrap()
        .iter()
        .copied()
        .filter(|(position, _)| *position != u64::MAX)
        .collect();
    assert_eq!(asked, [(1, true), (2, true), (3, true)]);
    assert!(*journal.head.lock().unwrap() >= 2, "once for each commit");
    assert!(*journal.bound.lock().unwrap() >= 3);

    // What was written is at the keys the layout named.
    let read = |key: &[u8]| db.db().get(key).unwrap();
    assert_eq!(read(b"journal-head"), Some(3u64.to_be_bytes().to_vec()));
    assert_eq!(read(&entry_key(1)), Some(b"one".to_vec()));
    assert_eq!(read(&entry_key(2)), Some(b"two".to_vec()));
    assert_eq!(read(&entry_key(3)), Some(b"three".to_vec()));
    assert_eq!(read(b"once/two"), Some(2u64.to_be_bytes().to_vec()));
}
