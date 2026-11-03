use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

use super::{push_metric_prelude, push_metric_sample};

/// A store walk touches every file, and stores with leaked compaction output
/// hold hundreds of thousands of them; refresh at most this often.
const STORE_SIZE_REFRESH: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ProcessMemory {
    pub(crate) resident_bytes: Option<u64>,
    /// macOS `phys_footprint`: the figure Activity Monitor and jetsam use,
    /// which counts compressed and swapped dirty pages that RSS omits.
    pub(crate) phys_footprint_bytes: Option<u64>,
    pub(crate) rss_anon_bytes: Option<u64>,
    pub(crate) rss_file_bytes: Option<u64>,
    pub(crate) rss_shmem_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StoreSize {
    pub(crate) bytes: u64,
    pub(crate) observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub(crate) struct ProcessMetricsSnapshot {
    pub(crate) memory: ProcessMemory,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) uptime: Duration,
    pub(crate) store: Option<StoreSize>,
}

pub(crate) fn sample_process_memory() -> ProcessMemory {
    platform_process_memory()
}

#[cfg(target_os = "linux")]
fn platform_process_memory() -> ProcessMemory {
    std::fs::read_to_string("/proc/self/status")
        .map(|status| parse_proc_status(&status))
        .unwrap_or_default()
}

#[cfg(target_os = "macos")]
fn platform_process_memory() -> ProcessMemory {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::zeroed();
    // SAFETY: info is writable storage for one rusage_info_v2, the layout the
    // RUSAGE_INFO_V2 flavor fills; libc declares the out-parameter as
    // `*mut rusage_info_t` although the kernel writes the struct itself.
    let status = unsafe {
        libc::proc_pid_rusage(
            std::process::id() as libc::c_int,
            libc::RUSAGE_INFO_V2,
            info.as_mut_ptr().cast(),
        )
    };
    if status != 0 {
        return ProcessMemory::default();
    }
    // SAFETY: proc_pid_rusage returned success, so it initialized info.
    let info = unsafe { info.assume_init() };
    ProcessMemory {
        resident_bytes: Some(info.ri_resident_size),
        phys_footprint_bytes: Some(info.ri_phys_footprint),
        ..ProcessMemory::default()
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_process_memory() -> ProcessMemory {
    ProcessMemory::default()
}

/// Reads the `kB` fields of Linux `/proc/<pid>/status`.
#[cfg_attr(
    not(any(target_os = "linux", test)),
    expect(dead_code, reason = "only Linux exposes /proc/self/status")
)]
fn parse_proc_status(status: &str) -> ProcessMemory {
    let mut memory = ProcessMemory::default();
    for line in status.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let Some(bytes) = value
            .trim()
            .strip_suffix("kB")
            .and_then(|kib| kib.trim().parse::<u64>().ok())
            .map(|kib| kib.saturating_mul(1024))
        else {
            continue;
        };
        match key {
            "VmRSS" => memory.resident_bytes = Some(bytes),
            "RssAnon" => memory.rss_anon_bytes = Some(bytes),
            "RssFile" => memory.rss_file_bytes = Some(bytes),
            "RssShmem" => memory.rss_shmem_bytes = Some(bytes),
            _ => {}
        }
    }
    memory
}

#[derive(Debug, Default)]
struct StoreSizeState {
    last: Option<StoreSize>,
    refreshing: bool,
    last_attempt: Option<Instant>,
}

/// Measures the store directory off the runtime workers. A scrape never waits
/// for the walk: it reads the last completed measurement and, when that is
/// older than [`STORE_SIZE_REFRESH`], starts one refresh in the background.
#[derive(Debug, Default)]
pub(crate) struct StoreSizeCache {
    dir: Option<PathBuf>,
    state: Mutex<StoreSizeState>,
}

impl StoreSizeCache {
    pub(crate) fn new(dir: Option<PathBuf>) -> Arc<Self> {
        Arc::new(Self {
            dir,
            state: Mutex::default(),
        })
    }

    pub(crate) fn observe(self: &Arc<Self>) -> Option<StoreSize> {
        let dir = self.dir.clone()?;
        let mut state = self.state.lock().ok()?;
        let due = state
            .last_attempt
            .is_none_or(|attempt| attempt.elapsed() >= STORE_SIZE_REFRESH);
        if due && !state.refreshing {
            state.refreshing = true;
            state.last_attempt = Some(Instant::now());
            let cache = self.clone();
            tokio::task::spawn_blocking(move || {
                let measured = directory_size(&dir);
                if let Ok(mut state) = cache.state.lock() {
                    state.refreshing = false;
                    match measured {
                        Ok(bytes) => {
                            state.last = Some(StoreSize {
                                bytes,
                                observed_at: Utc::now(),
                            });
                        }
                        Err(error) => tracing::warn!(
                            store = %dir.display(),
                            "measuring store size for metrics failed: {error}"
                        ),
                    }
                }
            });
        }
        state.last
    }
}

/// Sums apparent file sizes without following symlinks. Entries that vanish
/// mid-walk (compaction removes files continuously) are skipped; only an
/// unreadable root is an error.
fn directory_size(root: &Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    let mut pending = vec![std::fs::read_dir(root)?];
    while let Some(entries) = pending.pop() {
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                if let Ok(child) = std::fs::read_dir(entry.path()) {
                    pending.push(child);
                }
            } else if metadata.is_file() {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    Ok(total)
}

pub(super) fn render_process_metrics(
    lines: &mut Vec<String>,
    snapshot: Option<&ProcessMetricsSnapshot>,
) {
    let memory = snapshot.map(|snapshot| snapshot.memory).unwrap_or_default();
    for (name, help, value) in [
        (
            "process_resident_memory_bytes",
            "Resident set size of the gents process.",
            memory.resident_bytes,
        ),
        (
            "gents_process_phys_footprint_bytes",
            "macOS physical footprint of the gents process, including compressed and swapped dirty memory.",
            memory.phys_footprint_bytes,
        ),
        (
            "gents_process_rss_anon_bytes",
            "Linux anonymous resident memory (RssAnon) of the gents process.",
            memory.rss_anon_bytes,
        ),
        (
            "gents_process_rss_file_bytes",
            "Linux file-backed resident memory (RssFile) of the gents process.",
            memory.rss_file_bytes,
        ),
        (
            "gents_process_rss_shmem_bytes",
            "Linux shared resident memory (RssShmem) of the gents process.",
            memory.rss_shmem_bytes,
        ),
    ] {
        push_metric_prelude(lines, name, help);
        if let Some(value) = value {
            push_metric_sample(lines, name, &[], value);
        }
    }

    push_metric_prelude(
        lines,
        "process_start_time_seconds",
        "Unix time the gents HTTP runtime started.",
    );
    if let Some(snapshot) = snapshot {
        push_metric_sample(
            lines,
            "process_start_time_seconds",
            &[],
            snapshot.started_at.timestamp(),
        );
    }
    push_metric_prelude(
        lines,
        "gents_process_uptime_seconds",
        "Seconds since the gents HTTP runtime started.",
    );
    if let Some(snapshot) = snapshot {
        push_metric_sample(
            lines,
            "gents_process_uptime_seconds",
            &[],
            snapshot.uptime.as_secs(),
        );
    }

    let store = snapshot.and_then(|snapshot| snapshot.store);
    push_metric_prelude(
        lines,
        "gents_store_size_bytes",
        "Apparent size of the store directory; measured at most every five minutes and absent until the first measurement completes.",
    );
    if let Some(store) = store {
        push_metric_sample(lines, "gents_store_size_bytes", &[], store.bytes);
    }
    push_metric_prelude(
        lines,
        "gents_store_size_observed_at_seconds",
        "Unix time gents_store_size_bytes was measured.",
    );
    if let Some(store) = store {
        push_metric_sample(
            lines,
            "gents_store_size_observed_at_seconds",
            &[],
            store.observed_at.timestamp(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_status_fields_parse_as_bytes() {
        let status = "Name:\tgents\nVmPeak:\t  900 kB\nVmRSS:\t  2048 kB\nRssAnon:\t  1536 kB\nRssFile:\t  500 kB\nRssShmem:\t  12 kB\nThreads:\t42\n";
        assert_eq!(
            parse_proc_status(status),
            ProcessMemory {
                resident_bytes: Some(2048 * 1024),
                phys_footprint_bytes: None,
                rss_anon_bytes: Some(1536 * 1024),
                rss_file_bytes: Some(500 * 1024),
                rss_shmem_bytes: Some(12 * 1024),
            }
        );
        assert_eq!(
            parse_proc_status("Name:\tgents\n"),
            ProcessMemory::default()
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn this_process_reports_resident_memory() {
        let memory = sample_process_memory();
        assert!(memory.resident_bytes.is_some_and(|bytes| bytes > 0));
        #[cfg(target_os = "macos")]
        assert!(memory.phys_footprint_bytes.is_some_and(|bytes| bytes > 0));
        #[cfg(target_os = "linux")]
        assert!(memory.rss_anon_bytes.is_some_and(|bytes| bytes > 0));
    }

    #[test]
    fn directory_size_sums_nested_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), vec![0u8; 100]).unwrap();
        std::fs::create_dir(dir.path().join("nested")).unwrap();
        std::fs::write(dir.path().join("nested").join("b"), vec![0u8; 23]).unwrap();
        assert_eq!(directory_size(dir.path()).unwrap(), 123);
        assert!(directory_size(&dir.path().join("missing")).is_err());
    }

    #[tokio::test]
    async fn store_size_cache_measures_once_per_refresh_window() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("sst"), vec![0u8; 64]).unwrap();
        let cache = StoreSizeCache::new(Some(dir.path().to_path_buf()));
        assert_eq!(cache.observe(), None, "the first scrape starts the walk");
        let measured = loop {
            if let Some(size) = cache.state.lock().unwrap().last {
                break size;
            }
            tokio::task::yield_now().await;
        };
        assert_eq!(measured.bytes, 64);
        std::fs::write(dir.path().join("sst2"), vec![0u8; 64]).unwrap();
        assert_eq!(
            cache.observe().map(|size| size.bytes),
            Some(64),
            "a fresh measurement is reused inside the refresh window"
        );
        assert!(!cache.state.lock().unwrap().refreshing);
        assert_eq!(StoreSizeCache::new(None).observe(), None);
    }

    #[test]
    fn process_metrics_render_known_values_and_omit_unknown() {
        let mut lines = Vec::new();
        render_process_metrics(
            &mut lines,
            Some(&ProcessMetricsSnapshot {
                memory: ProcessMemory {
                    resident_bytes: Some(4096),
                    phys_footprint_bytes: Some(8192),
                    ..ProcessMemory::default()
                },
                started_at: DateTime::from_timestamp(1_800_000_000, 0).unwrap(),
                uptime: Duration::from_secs(90),
                store: Some(StoreSize {
                    bytes: 1_234,
                    observed_at: DateTime::from_timestamp(1_800_000_060, 0).unwrap(),
                }),
            }),
        );
        let rendered = lines.join("\n");
        for sample in [
            "process_resident_memory_bytes 4096",
            "gents_process_phys_footprint_bytes 8192",
            "process_start_time_seconds 1800000000",
            "gents_process_uptime_seconds 90",
            "gents_store_size_bytes 1234",
            "gents_store_size_observed_at_seconds 1800000060",
        ] {
            assert!(rendered.lines().any(|line| line == sample), "{sample}");
        }
        assert!(rendered.contains("# TYPE gents_process_rss_anon_bytes gauge"));
        assert!(!rendered
            .lines()
            .any(|line| line.starts_with("gents_process_rss_anon_bytes ")));
        super::super::tests::assert_valid_exposition(&rendered, true);
    }
}
