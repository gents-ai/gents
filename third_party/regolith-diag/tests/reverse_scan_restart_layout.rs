//! Reverse iteration must not trust how a block was written.
//!
//! Stepping backwards inside a block replays key reconstruction forward from
//! the restart point that covers the target entry. The obvious way to find
//! that point is to divide the entry index by `RESTART_INTERVAL` and trust
//! that restart `n` sits at entry `n * RESTART_INTERVAL`. Nothing enforces
//! that pairing: block validation checks only that restart offsets increase,
//! land on entry boundaries, and share no key prefix. A file that says
//! otherwise would send the replay past the end of the entry region, where it
//! panicked, or leave it on the wrong entry, where it said nothing.
//!
//! Two things are held here. First, that a reverse walk is exactly a forward
//! walk backwards, over enough entries to cross many restart points and
//! several blocks. Second, that damage anywhere in a file's restart region
//! produces an error or correct data, never a panic.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// the filesystem, which does not exist there.
#![cfg(not(target_arch = "wasm32"))]

use regolith::{Db, Options, WriteBatch};

/// Small blocks so a modest number of rows spans many restart points and
/// more than one block.
fn blocky_options() -> Options {
    Options::default()
        .write_buffer_size(256 * 1024)
        .block_size(4 * 1024)
        .block_cache_size(0)
        .target_file_size(512 * 1024)
        .max_background_compactions(0)
}

const KEYS: u64 = 5_000;

fn build(dir: &std::path::Path) {
    let db = Db::open(dir, blocky_options()).unwrap();
    let value = [b'v'; 64];
    let mut batch = WriteBatch::new();
    for i in 0..KEYS {
        batch.put(&i.to_be_bytes(), &value);
        if batch.buffered_bytes() >= 64 * 1024 {
            let full = std::mem::take(&mut batch);
            regolith::through_stalls(|| db.write(full.clone())).unwrap();
        }
    }
    regolith::through_stalls(|| db.write(batch.clone())).unwrap();
    db.flush().unwrap();
    db.close().unwrap();
}

fn sst_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|s| s.to_str()) == Some("sst") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn a_reverse_walk_is_the_forward_walk_backwards() {
    let dir = tempfile::tempdir().unwrap();
    build(dir.path());
    let db = Db::open(dir.path(), blocky_options()).unwrap();

    let forward: Vec<Vec<u8>> = db
        .scan_stream(None, None)
        .unwrap()
        .map(|entry| entry.unwrap().0)
        .collect();
    assert_eq!(forward.len() as u64, KEYS);

    let mut it = db.iter();
    it.seek_to_last();
    let mut backward = Vec::new();
    while it.valid() {
        backward.push(it.key().unwrap().to_vec());
        it.prev();
    }
    it.status().expect("an intact store must walk back clean");

    backward.reverse();
    assert_eq!(
        backward, forward,
        "reverse iteration must visit exactly the forward sequence, in reverse"
    );
}

#[test]
fn a_reverse_walk_from_every_position_lands_on_the_right_key() {
    let dir = tempfile::tempdir().unwrap();
    build(dir.path());
    let db = Db::open(dir.path(), blocky_options()).unwrap();

    // Seek to a key, step back one, and check it is the key before it. Done
    // across the whole range so the step crosses restart points and block
    // boundaries at every alignment rather than only at the convenient ones.
    for i in (1..KEYS).step_by(37) {
        let target = i.to_be_bytes();
        let mut it = db.iter();
        it.seek(&target);
        assert!(it.valid(), "seek to {i} must land");
        assert_eq!(it.key().unwrap(), &target[..]);
        it.prev();
        assert!(it.valid(), "there is a key before {i}");
        assert_eq!(
            it.key().unwrap(),
            &(i - 1).to_be_bytes()[..],
            "stepping back from {i} must land on {}",
            i - 1
        );
    }
}

#[test]
fn damage_in_the_restart_region_never_panics_a_reverse_walk() {
    // Walk a window of byte positions near the end of the entry area, where
    // the restart array and its count live, and flip each one. Every trial
    // must end in an error or in correct data. A panic is the failure this
    // test exists to catch, and it is reported as one because a panic in a
    // reverse walk over a damaged file would abort the process rather than
    // fail the read.
    let source = tempfile::tempdir().unwrap();
    build(source.path());
    let files = sst_files(source.path());
    assert!(!files.is_empty(), "the load must have produced an SSTable");
    let pristine: Vec<(std::path::PathBuf, Vec<u8>)> = files
        .iter()
        .map(|p| (p.clone(), std::fs::read(p).unwrap()))
        .collect();

    let victim = &pristine[0];
    let len = victim.1.len();
    let mut trials = 0;
    for offset in (len / 2..len.saturating_sub(64)).step_by(101) {
        for bit in [0u32, 3, 7] {
            let dir = tempfile::tempdir().unwrap();
            for (path, bytes) in &pristine {
                let rel = path.strip_prefix(source.path()).unwrap();
                let dest = dir.path().join(rel);
                std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
                let mut bytes = bytes.clone();
                if path == &victim.0 {
                    bytes[offset] ^= 1 << bit;
                }
                std::fs::write(&dest, &bytes).unwrap();
            }
            for entry in std::fs::read_dir(source.path()).unwrap().flatten() {
                let p = entry.path();
                if p.is_file() {
                    let _ = std::fs::copy(&p, dir.path().join(p.file_name().unwrap()));
                }
            }

            trials += 1;
            // Opening may fail, which is a fine outcome for a damaged file.
            let Ok(db) = Db::open(dir.path(), blocky_options()) else {
                continue;
            };
            let mut it = db.iter();
            it.seek_to_last();
            let mut steps = 0;
            while it.valid() && steps < KEYS + 10 {
                let _ = it.key();
                it.prev();
                steps += 1;
            }
            // Whatever it returned, it returned. The assertion is that
            // control reached here at all.
            let _ = it.status();
        }
    }
    assert!(trials > 0, "the sweep must have run at least one trial");
}
