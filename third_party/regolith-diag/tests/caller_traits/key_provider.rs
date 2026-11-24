//! `KeyProvider`: `current` is asked each time a file is created, and `key`
//! once for each key id a database needs, with the ids the provider and the
//! files name.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use regolith::{Db, KeyId, KeyMaterial, KeyProvider, MemEnv, Options};

/// Holds the keys `ids`, the last one current, and records every call.
struct Keys {
    ids: Vec<u32>,
    current: AtomicU32,
    current_asked: AtomicUsize,
    keys_asked: Mutex<Vec<KeyId>>,
}

impl Keys {
    fn new(ids: &[u32]) -> Arc<Self> {
        Arc::new(Self {
            ids: ids.to_vec(),
            current: AtomicU32::new(*ids.last().unwrap()),
            current_asked: AtomicUsize::new(0),
            keys_asked: Mutex::default(),
        })
    }

    /// How often `current` was asked since the last time this was asked.
    fn current_asked(&self) -> usize {
        self.current_asked.swap(0, Ordering::Relaxed)
    }

    /// The ids `key` was asked for, sorted.
    fn keys_asked(&self) -> Vec<KeyId> {
        let mut asked = self.keys_asked.lock().unwrap().clone();
        asked.sort_by_key(|id| id.0);
        asked
    }
}

impl KeyProvider for Keys {
    fn current(&self) -> KeyId {
        self.current_asked.fetch_add(1, Ordering::Relaxed);
        KeyId(self.current.load(Ordering::Relaxed))
    }

    fn key(&self, id: KeyId) -> Option<KeyMaterial> {
        self.keys_asked.lock().unwrap().push(id);
        self.ids
            .contains(&id.0)
            .then(|| KeyMaterial::new([id.0 as u8; 32]))
    }
}

#[test]
fn current_is_asked_for_each_new_file_and_each_key_once() {
    let env = Arc::new(MemEnv::new());
    let open = |keys: &Arc<Keys>| {
        let options = Options::memory()
            .env(env.clone())
            .key_provider(keys.clone());
        Db::open("sealed", options).unwrap()
    };

    let one = Keys::new(&[1]);
    let db = open(&one);
    assert!(one.current_asked() > 0, "the open, and its first log");
    // A table, and the manifest batch that names it.
    db.put(b"a", b"1").unwrap();
    db.flush().unwrap();
    assert!(one.current_asked() > 0, "a flush");
    db.put(b"a", b"2").unwrap();
    db.flush().unwrap();
    db.compact_range(None, None).wait().unwrap();
    assert!(one.current_asked() > 0, "a compaction");
    db.close().unwrap();
    drop(db);
    // However many files that was, the one key was asked for once.
    assert_eq!(one.keys_asked(), [KeyId(1)]);

    // A rotated provider: the key the files name, and the one new files
    // are sealed under, each asked for once.
    let two = Keys::new(&[1, 2]);
    let db = open(&two);
    assert_eq!(db.get(b"a").unwrap(), Some(b"2".to_vec()));
    db.put(b"b", b"3").unwrap();
    db.flush().unwrap();
    assert!(two.current_asked() > 0);
    assert_eq!(two.keys_asked(), [KeyId(1), KeyId(2)]);
}
