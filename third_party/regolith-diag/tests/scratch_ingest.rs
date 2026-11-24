#![cfg(not(target_arch = "wasm32"))]

use regolith::{Db, IngestOptions, Options, SstFileWriter};
use tempfile::TempDir;

fn build(path: &std::path::Path, batch: usize, opts: &Options) {
    let mut w = SstFileWriter::create(path, opts).unwrap();
    for i in 0..64 {
        w.put(
            format!("ing_{batch:04}_{i:03}").as_bytes(),
            format!("v{batch}").as_bytes(),
        )
        .unwrap();
    }
    w.finish().unwrap();
}

#[test]
fn no_writers_at_all() {
    let dir = TempDir::new().unwrap();
    let staging = TempDir::new().unwrap();
    let opts = Options::default().write_buffer_size(32 * 1024);
    let db = Db::open(dir.path(), opts.clone()).unwrap();

    for batch in 0..5usize {
        let path = staging.path().join(format!("ing_{batch}.sst"));
        build(&path, batch, &opts);
        db.ingest_external_files(
            std::slice::from_ref(&path),
            IngestOptions {
                snapshot_consistency: false,
                ..IngestOptions::default()
            },
        )
        .wait()
        .unwrap();
        let missing: Vec<usize> = (0..64)
            .filter(|i| {
                db.get(format!("ing_{batch:04}_{i:03}").as_bytes())
                    .unwrap()
                    .is_none()
            })
            .collect();
        let scanned = db.scan(None, None).unwrap().len();
        println!(
            "after ingest {batch}: missing_via_get={} scanned={scanned} horizon_props ok",
            missing.len()
        );
        println!("{}", db.get_property("regolith.sstables").unwrap());
    }
    // And every earlier batch is still there.
    for batch in 0..5usize {
        let missing = (0..64)
            .filter(|i| {
                db.get(format!("ing_{batch:04}_{i:03}").as_bytes())
                    .unwrap()
                    .is_none()
            })
            .count();
        println!("final: batch {batch} missing_via_get={missing}");
    }
    println!("scan len = {}", db.scan(None, None).unwrap().len());
}

#[test]
fn one_plain_write_between_ingests() {
    let dir = TempDir::new().unwrap();
    let staging = TempDir::new().unwrap();
    let opts = Options::default().write_buffer_size(32 * 1024);
    let db = Db::open(dir.path(), opts.clone()).unwrap();

    for batch in 0..5usize {
        regolith::through_stalls(|| db.put(format!("plain{batch}").as_bytes(), b"p")).unwrap();
        let path = staging.path().join(format!("ing_{batch}.sst"));
        build(&path, batch, &opts);
        db.ingest_external_files(
            std::slice::from_ref(&path),
            IngestOptions {
                snapshot_consistency: false,
                ..IngestOptions::default()
            },
        )
        .wait()
        .unwrap();
        for i in 0..64 {
            let k = format!("ing_{batch:04}_{i:03}");
            assert_eq!(
                db.get(k.as_bytes()).unwrap(),
                Some(format!("v{batch}").into_bytes()),
                "batch {batch} key {i} invisible right after ingest"
            );
        }
    }
    println!("{}", db.get_property("regolith.sstables").unwrap());
}

/// Same five ingests, but each one through a freshly opened `Db` so the
/// block cache starts cold. If the data survives here and not above, the
/// fault is the cache, not the writer or the file.
#[test]
fn cold_cache_per_ingest() {
    let dir = TempDir::new().unwrap();
    let staging = TempDir::new().unwrap();
    let opts = Options::default().write_buffer_size(32 * 1024);
    for batch in 0..5usize {
        let path = staging.path().join(format!("ing_{batch}.sst"));
        build(&path, batch, &opts);
        let db = Db::open(dir.path(), opts.clone()).unwrap();
        db.ingest_external_files(
            std::slice::from_ref(&path),
            IngestOptions {
                snapshot_consistency: false,
                ..IngestOptions::default()
            },
        )
        .wait()
        .unwrap();
        db.close().unwrap();
    }
    let db = Db::open(dir.path(), opts).unwrap();
    for batch in 0..5usize {
        let missing = (0..64)
            .filter(|i| {
                db.get(format!("ing_{batch:04}_{i:03}").as_bytes())
                    .unwrap()
                    .is_none()
            })
            .count();
        println!("cold cache: batch {batch} missing_via_get={missing}");
    }
    println!(
        "cold cache: scan len = {}",
        db.scan(None, None).unwrap().len()
    );
}
