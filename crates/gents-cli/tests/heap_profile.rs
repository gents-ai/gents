#![cfg(all(
    feature = "heap-profiling",
    any(target_os = "linux", target_os = "macos")
))]

#[global_allocator]
static ALLOCATOR: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[test]
fn profiling_child() {
    if std::env::var_os("GENTS_HEAP_PROFILE_TEST_CHILD").is_none() {
        return;
    }
    let allocations: Vec<Vec<u8>> = (0..64).map(|_| vec![1; 128 * 1024]).collect();
    std::hint::black_box(allocations);
}

#[test]
fn profiling_build_writes_sampled_heap_dumps() {
    let directory = tempfile::tempdir().unwrap();
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "profiling_child", "--nocapture"])
        .current_dir(directory.path())
        .env("GENTS_HEAP_PROFILE_TEST_CHILD", "1")
        .env(
            "_RJEM_MALLOC_CONF",
            "prof:true,lg_prof_interval:20,lg_prof_sample:10,prof_prefix:heap",
        )
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(std::fs::read_dir(directory.path()).unwrap().any(|entry| {
        entry
            .unwrap()
            .path()
            .extension()
            .is_some_and(|extension| extension == "heap")
    }));
}
