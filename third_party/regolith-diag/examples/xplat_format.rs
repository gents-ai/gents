//! Cross-target on-disk format probe.
//!
//! Writes or verifies a deterministic dataset with byte-identical
//! options on every target, so a database produced natively can be
//! read under `wasm32-wasip1` and the reverse. Any divergence in the
//! on-disk format shows up as a read miss or a value mismatch.
//!
//! ```sh
//! cargo run --release --example xplat_format -- --dir DIR --mode write
//! wasmtime run --dir=DIR::/data target/wasm32-wasip1/release/examples/xplat_format.wasm \
//!     -- --dir /data --mode verify
//! ```

use regolith::{Db, Error, Options, WriteBatch};

const N: usize = 4_000;

/// Options pinned identically on every target so the bytes on disk are
/// a function of the data alone.
fn opts() -> Options {
    Options::default()
        .write_buffer_size(32 * 1024)
        .target_file_size(64 * 1024)
        .level_base_bytes(256 * 1024)
        .block_size(4 * 1024)
        .block_cache_size(0)
        .max_background_compactions(0)
        .bloom_bits_per_key(10)
}

fn key(i: usize) -> Vec<u8> {
    format!("key/{i:09}").into_bytes()
}

/// Deterministic, non-uniform value so a byte-order or framing bug
/// cannot be masked by a constant fill.
fn value(i: usize) -> Vec<u8> {
    let len = 32 + (i * 7) % 200;
    (0..len).map(|j| ((i * 31 + j * 17) % 256) as u8).collect()
}

fn deleted(i: usize) -> bool {
    i % 11 == 3
}

fn in_range_delete(i: usize) -> bool {
    (1000..1100).contains(&i)
}

fn overwritten(i: usize) -> bool {
    i % 17 == 5
}

fn expected(i: usize) -> Option<Vec<u8>> {
    if deleted(i) || in_range_delete(i) {
        None
    } else if overwritten(i) {
        Some(value(i + 1))
    } else {
        Some(value(i))
    }
}

fn write(dir: &str) -> Result<(), String> {
    let db = Db::open(dir, opts()).map_err(|e| format!("open: {e:?}"))?;
    for i in 0..N {
        db.put(&key(i), &value(i))
            .map_err(|e| format!("put {i}: {e:?}"))?;
    }
    let mut batch = WriteBatch::new();
    for i in 0..N {
        if overwritten(i) {
            batch.put(&key(i), &value(i + 1));
        }
        if deleted(i) {
            batch.delete(&key(i));
        }
    }
    db.write(batch).map_err(|e| format!("batch: {e:?}"))?;
    db.delete_range(&key(1000), &key(1100))
        .map_err(|e| format!("delete_range: {e:?}"))?;
    db.flush().map_err(|e| format!("flush: {e:?}"))?;
    db.compact_range(None, None)
        .wait()
        .map_err(|e| format!("compact_range: {e:?}"))?;
    db.close().map_err(|e| format!("close: {e:?}"))?;
    println!("WROTE {N} records to {dir}");
    Ok(())
}

fn verify(dir: &str) -> Result<(), String> {
    let db = Db::open(dir, opts()).map_err(|e| format!("open: {e:?}"))?;
    let mut checked = 0usize;
    for i in 0..N {
        let got = db.get(&key(i)).map_err(|e| format!("get {i}: {e:?}"))?;
        let want = expected(i);
        if got != want {
            return Err(format!(
                "record {i}: expected {:?} bytes, got {:?} bytes",
                want.as_ref().map(|v| v.len()),
                got.as_ref().map(|v| v.len())
            ));
        }
        checked += 1;
    }
    let live = (0..N).filter(|i| expected(*i).is_some()).count();
    let scanned = db
        .scan(Some(&key(0)[..]), Some(&key(N)[..]))
        .map_err(|e| format!("scan: {e:?}"))?
        .len();
    if scanned != live {
        return Err(format!("scan returned {scanned} rows, expected {live}"));
    }
    db.close().map_err(|e| format!("close: {e:?}"))?;
    println!("VERIFIED {checked} records, scan={scanned} in {dir}");
    Ok(())
}

/// Two `Db` handles on one directory in a single process. On a target
/// with `Capabilities::file_lock == false` nothing stops the second
/// open, so this reports what actually happens rather than assuming.
fn double_open(dir: &str) -> Result<(), String> {
    let a = Db::open(dir, opts()).map_err(|e| format!("first open: {e:?}"))?;
    a.put(b"shared", b"from-a")
        .map_err(|e| format!("a.put: {e:?}"))?;
    match Db::open(dir, opts()) {
        Err(e) => {
            println!("SECOND OPEN REJECTED: {e:?}");
            a.close().map_err(|e| format!("a.close: {e:?}"))?;
            Ok(())
        }
        Ok(b) => {
            println!("SECOND OPEN SUCCEEDED - two writers on one directory");
            b.put(b"shared", b"from-b")
                .map_err(|e| format!("b.put: {e:?}"))?;
            for i in 0..2_000usize {
                a.put(&key(i), &value(i))
                    .map_err(|e| format!("a.put {i}: {e:?}"))?;
                b.put(&key(i + 100_000), &value(i))
                    .map_err(|e| format!("b.put {i}: {e:?}"))?;
            }
            let a_sees = a.get(&key(100_000)).map_err(|e| format!("{e:?}"))?;
            let b_sees = b.get(&key(0)).map_err(|e| format!("{e:?}"))?;
            println!(
                "a sees b's key: {}   b sees a's key: {}",
                a_sees.is_some(),
                b_sees.is_some()
            );
            let ca = a.close();
            let cb = b.close();
            println!("close a -> {ca:?}   close b -> {cb:?}");
            let re = Db::open(dir, opts()).map_err(|e| format!("reopen: {e:?}"))?;
            let mut lost_a = 0usize;
            let mut lost_b = 0usize;
            for i in 0..2_000usize {
                if re.get(&key(i)).map_err(|e| format!("{e:?}"))?.is_none() {
                    lost_a += 1;
                }
                if re
                    .get(&key(i + 100_000))
                    .map_err(|e| format!("{e:?}"))?
                    .is_none()
                {
                    lost_b += 1;
                }
            }
            println!("after reopen: writer A lost {lost_a}/2000, writer B lost {lost_b}/2000");
            re.close().map_err(|e| format!("{e:?}"))?;
            Err(format!(
                "two concurrent writers were allowed; A lost {lost_a}, B lost {lost_b}"
            ))
        }
    }
}

/// Write a couple of records and exit without closing, leaving the
/// records only in the WAL. Simulates a crash.
fn crash_write(dir: &str) -> Result<(), String> {
    let db = Db::open(dir, opts()).map_err(|e| format!("open: {e:?}"))?;
    db.put(b"good_1", b"1").map_err(|e| format!("{e:?}"))?;
    db.put(b"good_2", b"2").map_err(|e| format!("{e:?}"))?;
    std::mem::forget(db);
    println!("CRASH_WRITE ok");
    Ok(())
}

/// Attempt to open and report the outcome in one stable line, so the
/// same directory can be classified identically on every target.
fn recover(dir: &str) -> Result<(), String> {
    match Db::open(dir, opts()) {
        Ok(db) => {
            let g1 = db.get(b"good_1").map_err(|e| format!("{e:?}"))?.is_some();
            let g2 = db.get(b"good_2").map_err(|e| format!("{e:?}"))?.is_some();
            println!("RECOVER open=OK good_1={g1} good_2={g2}");
            let _ = db.close();
            Ok(())
        }
        Err(Error::Corruption(e)) => {
            println!("RECOVER open=CORRUPTION kind={:?}", e.kind());
            Ok(())
        }
        Err(e) => {
            println!("RECOVER open=OTHER {e:?}");
            Ok(())
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut dir = String::from("/data");
    let mut mode = String::from("verify");
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--dir" => dir = it.next().cloned().unwrap_or_default(),
            "--mode" => mode = it.next().cloned().unwrap_or_default(),
            _ => {}
        }
    }
    let result = match mode.as_str() {
        "write" => write(&dir),
        "verify" => verify(&dir),
        "double-open" => double_open(&dir),
        "crash-write" => crash_write(&dir),
        "recover" => recover(&dir),
        other => Err(format!("unknown mode {other}")),
    };
    if let Err(e) = result {
        eprintln!("FAIL  {e}");
        std::process::exit(1);
    }
}
