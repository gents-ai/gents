use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use chrono::{DateTime, Utc};

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct ActiveManagedExecSnapshot {
    pub(crate) id: u64,
    pub(crate) pid: i32,
    pub(crate) argv0: String,
    pub(crate) tool_name: Option<String>,
    pub(crate) started_at: String,
    pub(crate) age_ms: i64,
}

#[derive(Debug, Clone)]
struct ActiveManagedExecRecord {
    id: u64,
    pid: i32,
    argv0: String,
    tool_name: Option<String>,
    started_at: DateTime<Utc>,
    started_instant: Instant,
}

static NEXT_EXEC_ID: AtomicU64 = AtomicU64::new(1);
static ACTIVE_EXECS: OnceLock<Mutex<BTreeMap<u64, ActiveManagedExecRecord>>> = OnceLock::new();

fn active_execs() -> &'static Mutex<BTreeMap<u64, ActiveManagedExecRecord>> {
    ACTIVE_EXECS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

pub(crate) fn active_executor_snapshots() -> Vec<ActiveManagedExecSnapshot> {
    active_execs()
        .lock()
        .expect("managed exec active registry mutex poisoned")
        .values()
        .map(|record| ActiveManagedExecSnapshot {
            id: record.id,
            pid: record.pid,
            argv0: record.argv0.clone(),
            tool_name: record.tool_name.clone(),
            started_at: record.started_at.to_rfc3339(),
            age_ms: i64::try_from(record.started_instant.elapsed().as_millis()).unwrap_or(i64::MAX),
        })
        .collect()
}

pub(super) struct ActiveExecGuard {
    id: u64,
}

impl ActiveExecGuard {
    pub(super) fn insert(pid: i32, argv0: String, tool_name: Option<String>) -> Self {
        let id = NEXT_EXEC_ID.fetch_add(1, Ordering::Relaxed);
        let record = ActiveManagedExecRecord {
            id,
            pid,
            argv0,
            tool_name,
            started_at: Utc::now(),
            started_instant: Instant::now(),
        };
        active_execs()
            .lock()
            .expect("managed exec active registry mutex poisoned")
            .insert(id, record);
        Self { id }
    }
}

impl Drop for ActiveExecGuard {
    fn drop(&mut self) {
        active_execs()
            .lock()
            .expect("managed exec active registry mutex poisoned")
            .remove(&self.id);
    }
}
