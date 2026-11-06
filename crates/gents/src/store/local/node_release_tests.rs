use std::sync::{Arc, Weak};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Set in the isolated child process, naming the file it reports to. Thread
/// and descriptor counts are process-wide, so concurrent tests in the parent
/// would make them meaningless.
const REPORT_ENV: &str = "GENTS_NODE_RELEASE_REPORT";
const TEST_NAME: &str =
    "store::local::node_release_tests::dropped_fixture_nodes_release_threads_and_descriptors";
const CYCLES: usize = 20;
/// Lazily started process-wide threads and descriptors (tracing, DNS, a
/// shared HTTP client) may appear once; a leaked HTTP-serving node costs
/// about 14 threads and 7 descriptors per cycle.
const SLACK: usize = 8;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct ProcessResources {
    threads: usize,
    fds: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct StageReport {
    stage: String,
    cycles: usize,
    retained_nodes: usize,
    baseline: ProcessResources,
    after: ProcessResources,
}

#[cfg(target_vendor = "apple")]
fn thread_count() -> usize {
    let mut info = std::mem::MaybeUninit::<libc::proc_taskinfo>::zeroed();
    let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_taskinfo>()).unwrap();
    // SAFETY: `info` is a writable buffer of exactly `size` bytes, the layout
    // PROC_PIDTASKINFO fills.
    let written = unsafe {
        libc::proc_pidinfo(
            libc::c_int::try_from(std::process::id()).unwrap(),
            libc::PROC_PIDTASKINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    assert_eq!(written, size, "proc_pidinfo(PROC_PIDTASKINFO)");
    // SAFETY: the call above filled the whole struct.
    usize::try_from(unsafe { info.assume_init() }.pti_threadnum).unwrap()
}

#[cfg(target_os = "linux")]
fn thread_count() -> usize {
    std::fs::read_dir("/proc/self/task").unwrap().count()
}

fn fd_count() -> usize {
    #[cfg(target_vendor = "apple")]
    let dir = "/dev/fd";
    #[cfg(target_os = "linux")]
    let dir = "/proc/self/fd";
    std::fs::read_dir(dir).unwrap().count()
}

fn sample() -> ProcessResources {
    ProcessResources {
        threads: thread_count(),
        fds: fd_count(),
    }
}

async fn settle_to(baseline: ProcessResources) -> ProcessResources {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let now = sample();
        if (now.threads <= baseline.threads && now.fds <= baseline.fds)
            || tokio::time::Instant::now() >= deadline
        {
            return now;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn released(node: &Weak<defra_node::Node>) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while node.strong_count() != 0 {
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    true
}

/// One warm-up cycle starts process-wide services before the baseline.
async fn measure<F, Fut>(stage: &str, mut cycle: F) -> StageReport
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Weak<defra_node::Node>>,
{
    released(&cycle().await).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let baseline = sample();
    let mut retained_nodes = 0;
    for _ in 0..CYCLES {
        if !released(&cycle().await).await {
            retained_nodes += 1;
        }
    }
    StageReport {
        stage: stage.to_owned(),
        cycles: CYCLES,
        retained_nodes,
        baseline,
        after: settle_to(baseline).await,
    }
}

async fn measure_fixtures() -> Vec<StageReport> {
    vec![
        measure("embedded_home", || async {
            let home = crate::eval::runner::embedded::EmbeddedHome::create_temp("node-release")
                .await
                .unwrap();
            Arc::downgrade(&home.node)
        })
        .await,
        measure("test_db", || async {
            let db = crate::support::test_db("node-release-db").await;
            Arc::downgrade(&db.node)
        })
        .await,
        measure("signed_test_node", || async {
            let (db, _user, _access, _binding) =
                crate::test_support::signed_test_node("node-release-signed").await;
            Arc::downgrade(&db.node)
        })
        .await,
    ]
}

#[tokio::test]
async fn dropped_fixture_nodes_release_threads_and_descriptors() {
    if let Some(report) = std::env::var_os(REPORT_ENV) {
        let reports = measure_fixtures().await;
        tokio::fs::write(report, serde_json::to_vec(&reports).unwrap())
            .await
            .unwrap();
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let report = dir.path().join("report.json");
    let output = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([TEST_NAME, "--exact", "--test-threads=1"])
        .env(REPORT_ENV, &report)
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "isolated measurement failed: {}\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let reports: Vec<StageReport> =
        serde_json::from_slice(&tokio::fs::read(&report).await.unwrap()).unwrap();
    assert_eq!(reports.len(), 3, "{reports:?}");
    for report in &reports {
        assert_eq!(report.retained_nodes, 0, "{report:?}");
        assert!(
            report.after.threads <= report.baseline.threads + SLACK
                && report.after.fds <= report.baseline.fds + SLACK,
            "dropped fixture nodes kept process resources: {report:?}"
        );
    }
}
