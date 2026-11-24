//! `KeyClassifier::classify`, the trait's one method: asked by an optimistic
//! transaction at `DefraLevel`, about the key as the caller wrote it, for
//! the keys it reads, writes, scans and appends to. No other level asks.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use regolith::{
    IsolationLevel, KeyClass, KeyClassifier, LogLayout, OptimisticTransactionDb, Options,
    TxnOptions,
};

/// Tells the class from the key's first bytes, and records every key it is
/// asked about.
#[derive(Default)]
struct ByPrefix(Mutex<BTreeSet<Vec<u8>>>);

impl ByPrefix {
    /// The keys asked about since the last time this was asked.
    fn asked(&self) -> BTreeSet<Vec<u8>> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

impl KeyClassifier for ByPrefix {
    fn classify(&self, key: &[u8]) -> KeyClass {
        self.0.lock().unwrap().insert(key.to_vec());
        if key.starts_with(b"blob/") {
            KeyClass::ContentAddressed
        } else if key.starts_with(b"feed/") {
            KeyClass::CommutativePrefix { len: 5 }
        } else if key.starts_with(b"journal") {
            KeyClass::Log
        } else {
            KeyClass::Ordinary
        }
    }
}

struct Journal;

impl LogLayout for Journal {
    fn head_key(&self) -> &[u8] {
        b"journal-head"
    }

    fn entry_key(&self, position: u64, out: &mut Vec<u8>) {
        out.extend_from_slice(b"journal/");
        out.extend_from_slice(&position.to_be_bytes());
    }

    fn max_entry_key_len(&self) -> usize {
        b"journal/".len() + 8
    }
}

fn keys(names: &[&[u8]]) -> BTreeSet<Vec<u8>> {
    names.iter().map(|name| name.to_vec()).collect()
}

#[test]
fn classify_is_asked_about_the_callers_keys_at_defra_level_only() {
    let classifier = Arc::new(ByPrefix::default());
    let db = OptimisticTransactionDb::open("classes", Options::memory())
        .unwrap()
        .with_policy(classifier.clone());
    db.db().put(b"feed/1", b"v").unwrap();
    db.db().put(b"row/1", b"v").unwrap();
    let journal: Arc<dyn LogLayout> = Arc::new(Journal);
    let entry_at = |position: u64| {
        let mut key = Vec::new();
        Journal.entry_key(position, &mut key);
        key
    };

    let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
    // A write asks about its key when it is made, to refuse a log key.
    txn.put(b"blob/9f2c", b"bytes").unwrap();
    txn.delete(b"row/2").unwrap();
    assert_eq!(classifier.asked(), keys(&[b"blob/9f2c", b"row/2"]));
    assert!(txn.put(b"journal-head", b"mine").is_err());
    assert_eq!(classifier.asked(), keys(&[b"journal-head"]));
    // An append asks about each key of its log.
    txn.append(&journal, b"entry", Some(b"journal-once/7"))
        .unwrap();
    assert_eq!(
        classifier.asked(),
        keys(&[b"journal-head", &entry_at(u64::MAX), b"journal-once/7"])
    );
    // A read and a scan ask nothing until the commit.
    assert_eq!(txn.get(b"row/1").unwrap(), Some(b"v".to_vec()));
    assert_eq!(txn.scan_stream(Some(b"feed/"), Some(b"feed0")).count(), 1);
    assert!(classifier.asked().is_empty());

    // The commit asks about what it read and put, and the start of the
    // stretch it scanned: never a key it only deleted.
    txn.commit().unwrap();
    assert_eq!(
        classifier.asked(),
        keys(&[b"blob/9f2c", b"feed/1", b"row/1"])
    );

    // The same work at another level asks nothing.
    let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
    txn.put(b"blob/1a", b"bytes").unwrap();
    assert_eq!(txn.get(b"row/1").unwrap(), Some(b"v".to_vec()));
    txn.append(&journal, b"entry", None).unwrap();
    txn.commit().unwrap();
    assert!(classifier.asked().is_empty());
}
