//! Measure the caller's allocations separately from background engine work.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use regolith::{CompressionType, Db, Error, Options};

thread_local! {
    static LARGEST: Cell<usize> = const { Cell::new(0) };
}

struct MeasuredAllocator;

// SAFETY: allocation operations are forwarded unchanged to System. The
// const-initialized, drop-free counter does not allocate or unwind.
unsafe impl GlobalAlloc for MeasuredAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = LARGEST.try_with(|size| size.set(size.get().max(layout.size())));
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let _ = LARGEST.try_with(|size| size.set(size.get().max(layout.size())));
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let _ = LARGEST.try_with(|size| size.set(size.get().max(new_size)));
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: MeasuredAllocator = MeasuredAllocator;

fn measure<T>(operation: impl FnOnce() -> T) -> (T, usize) {
    LARGEST.with(|size| size.set(0));
    let result = operation();
    (result, LARGEST.with(Cell::get))
}

#[test]
fn oversized_size_lookup_rejects_without_value_sized_allocation() {
    const VALUE_SIZE: usize = 17 * 1024 * 1024;
    const LIMIT: usize = 1024 * 1024;

    for compression in [
        CompressionType::None,
        CompressionType::Lz4,
        CompressionType::Snappy,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let options = || {
            Options::default()
                .compression(compression)
                .block_cache_size(0)
        };
        {
            let db = Db::open(directory.path(), options()).unwrap();
            db.put(b"oversized", &vec![42; VALUE_SIZE]).unwrap();
            db.flush().unwrap();
            db.close().unwrap();
        }

        if compression != CompressionType::None {
            let tables: Vec<_> = std::fs::read_dir(directory.path().join("sst"))
                .unwrap()
                .map(Result::unwrap)
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "sst"))
                .collect();
            assert_eq!(tables.len(), 1);
            assert!(tables[0].metadata().unwrap().len() < LIMIT as u64);
        }

        let db = Db::open(directory.path(), options()).unwrap();
        let snapshot = db.snapshot();
        let (result, largest) = measure(|| snapshot.get_size_with_limit(b"oversized", LIMIT));
        assert!(
            matches!(
                result,
                Err(Error::DataBlockLimitExceeded {
                    max_data_block_bytes: LIMIT
                })
            ),
            "{compression:?}: {result:?}"
        );
        assert!(
            largest < LIMIT,
            "{compression:?}: allocated {largest} bytes"
        );

        // The old API remains exact and unrestricted. This also verifies
        // that the measured path really reaches an oversized on-disk block.
        let (result, largest) = measure(|| snapshot.get_size(b"oversized"));
        assert_eq!(result.unwrap(), Some(VALUE_SIZE));
        assert!(largest >= VALUE_SIZE, "{compression:?}: {largest}");
        drop(snapshot);
        db.close().unwrap();
    }
}
