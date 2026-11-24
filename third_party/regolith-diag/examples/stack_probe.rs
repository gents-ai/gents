//! Runs one regolith workload on a thread with a caller-chosen stack size.
//!
//! A stack overflow aborts the process, so the exit status is the
//! verdict: exit 0 means the workload fits, 134 means it does not.
//! This checks the README's "16 KiB of stack" figure against paths the
//! calibrated painting harness did not cover.
//!
//! `--stack` is a floor, not an exact size. `std::thread::Builder`
//! raises anything below the platform minimum (`PTHREAD_STACK_MIN`,
//! 16 KiB on glibc x86_64) and rounds up to a page, so a request under
//! that minimum is silently granted more and this harness cannot
//! resolve anything finer. The size actually requested is printed with
//! each run so a passing result at, say, 4 KiB is not read as proof
//! that 4 KiB is enough.
//!
//! Run `--workload control` first: it recurses through fixed 4 KiB
//! frames and must report 134 at every size below 1 MiB and 0 at
//! 1 MiB. A control that passes everywhere means the detector is not
//! working and no other row on the sweep means anything.

use regolith::{Db, Options, WriteBatch};

fn opts() -> Options {
    Options::default()
        .write_buffer_size(32 * 1024)
        .target_file_size(48 * 1024)
        .level_base_bytes(128 * 1024)
        .block_cache_size(0)
        .max_background_compactions(0)
        .max_key_size(8 * 1024 * 1024)
        .max_value_size(8 * 1024 * 1024)
}

/// Build a multi-level database with many overlapping L0 files.
fn seed(dir: &str) {
    let db = Db::open(dir, opts()).expect("seed open");
    for round in 0..6 {
        for i in 0..6_000usize {
            let k = format!("key/{:09}", (i * 7 + round) % 20_000);
            db.put(k.as_bytes(), &[(i % 251) as u8; 128])
                .expect("seed put");
        }
        db.flush().expect("seed flush");
    }
    db.close().expect("seed close");
}

fn workload(name: &str, dir: &str) {
    match name {
        "open" => {
            let db = Db::open(dir, opts()).expect("open");
            db.close().expect("close");
        }
        "get" => {
            let db = Db::open(dir, opts()).expect("open");
            for i in 0..500usize {
                let k = format!("key/{i:09}");
                let _ = db.get(k.as_bytes()).expect("get");
            }
            db.close().expect("close");
        }
        "iter" => {
            let db = Db::open(dir, opts()).expect("open");
            let mut it = db.iter();
            it.seek(b"key/000005000");
            let mut n = 0;
            while it.valid() {
                let _ = it.key();
                it.next();
                n += 1;
                if n > 5_000 {
                    break;
                }
            }
            drop(it);
            db.close().expect("close");
        }
        "compact" => {
            let db = Db::open(dir, opts()).expect("open");
            db.compact_range(None, None).wait().expect("compact_range");
            db.close().expect("close");
        }
        "long_key" => {
            let db = Db::open(dir, opts()).expect("open");
            let k = vec![b'z'; 1024 * 1024];
            db.put(&k, b"v").expect("put long key");
            let _ = db.get(&k).expect("get long key");
            db.close().expect("close");
        }
        "big_batch" => {
            let db = Db::open(dir, opts()).expect("open");
            let mut b = WriteBatch::new();
            for i in 0..20_000usize {
                b.put(format!("b/{i:09}").as_bytes(), &[7u8; 64]);
            }
            db.write(b).expect("write batch");
            db.close().expect("close");
        }
        "scan_page" => {
            let db = Db::open(dir, opts()).expect("open");
            let mut start: Option<Vec<u8>> = None;
            for _ in 0..20 {
                let page = db
                    .scan_page(start.as_deref(), None, 512)
                    .expect("scan_page");
                match page.next_start {
                    Some(k) => start = Some(k),
                    None => break,
                }
            }
            db.close().expect("close");
        }
        "control" => {
            // Harness control: must overflow, proving the probe can
            // actually detect a blown stack.
            fn burn(d: usize) -> u64 {
                let mut pad = [0u8; 4096];
                pad[d % 4096] = d as u8;
                let pad = std::hint::black_box(pad);
                if d == 0 {
                    return pad[0] as u64;
                }
                pad[0] as u64 + burn(d - 1)
            }
            println!("control sum {}", burn(64));
        }
        other => panic!("unknown workload {other}"),
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let mut dir = String::new();
    let mut name = String::new();
    let mut stack = 0usize;
    let mut do_seed = false;
    let mut it = a.iter().skip(1);
    while let Some(x) = it.next() {
        match x.as_str() {
            "--dir" => dir = it.next().cloned().unwrap_or_default(),
            "--workload" => name = it.next().cloned().unwrap_or_default(),
            "--stack" => stack = it.next().and_then(|s| s.parse().ok()).unwrap_or(0),
            "--seed" => do_seed = true,
            _ => {}
        }
    }
    if do_seed {
        seed(&dir);
        println!("seeded");
        return;
    }
    println!("requested stack {stack} B (the platform floor may raise it)");
    let h = std::thread::Builder::new()
        .stack_size(stack)
        .spawn(move || workload(&name, &dir))
        .expect("spawn");
    match h.join() {
        Ok(()) => println!("OK"),
        Err(_) => {
            eprintln!("PANIC");
            std::process::exit(2);
        }
    }
}
