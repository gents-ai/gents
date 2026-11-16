use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(windows)]
use std::sync::Arc;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ManagedExecKind {
    ForegroundCommand,
    PersistentService,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct ActiveManagedExecSnapshot {
    pub(crate) id: u64,
    pub(crate) pid: i32,
    pub(crate) argv0: String,
    pub(crate) tool_name: Option<String>,
    pub(crate) kind: ManagedExecKind,
    pub(crate) started_at: String,
    pub(crate) age_ms: i64,
}

#[derive(Clone)]
struct ActiveManagedExecRecord {
    id: u64,
    pid: i32,
    argv0: String,
    tool_name: Option<String>,
    kind: ManagedExecKind,
    started_at: DateTime<Utc>,
    started_instant: Instant,
    #[cfg(windows)]
    failed_service: Option<Arc<super::job_object::FailedJobOwner>>,
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
            kind: record.kind,
            started_at: record.started_at.to_rfc3339(),
            age_ms: i64::try_from(record.started_instant.elapsed().as_millis()).unwrap_or(i64::MAX),
        })
        .collect()
}

pub(super) struct ActiveExecGuard {
    id: u64,
}

impl ActiveExecGuard {
    pub(super) fn insert(
        pid: i32,
        argv0: String,
        tool_name: Option<String>,
        kind: ManagedExecKind,
    ) -> Self {
        let id = NEXT_EXEC_ID.fetch_add(1, Ordering::Relaxed);
        let record = ActiveManagedExecRecord {
            id,
            pid,
            argv0,
            tool_name,
            kind,
            started_at: Utc::now(),
            started_instant: Instant::now(),
            #[cfg(windows)]
            failed_service: None,
        };
        active_execs()
            .lock()
            .expect("managed exec active registry mutex poisoned")
            .insert(id, record);
        Self { id }
    }
}

/// Retain a failed service's exact OS handles even if its setup future is cancelled.
#[cfg(windows)]
pub(super) fn retain_failed_service(
    owner: Arc<super::job_object::FailedJobOwner>,
    argv0: String,
    tool_name: Option<String>,
    kind: ManagedExecKind,
) -> u64 {
    let id = NEXT_EXEC_ID.fetch_add(1, Ordering::Relaxed);
    let record = ActiveManagedExecRecord {
        id,
        pid: owner.pid().unwrap_or(0),
        argv0,
        tool_name,
        kind,
        started_at: Utc::now(),
        started_instant: Instant::now(),
        failed_service: Some(owner),
    };
    active_execs()
        .lock()
        .expect("managed exec active registry mutex poisoned")
        .insert(id, record);
    id
}

/// Remove only the same owner whose process and job were observed stopped.
#[cfg(windows)]
pub(super) async fn stop_failed_service(id: u64) -> bool {
    let owner = active_execs()
        .lock()
        .expect("managed exec active registry mutex poisoned")
        .get(&id)
        .and_then(|record| record.failed_service.clone());
    let Some(owner) = owner else { return false };
    if !owner.stop_and_confirm().await {
        return false;
    }
    let mut records = active_execs()
        .lock()
        .expect("managed exec active registry mutex poisoned");
    if records
        .get(&id)
        .and_then(|record| record.failed_service.as_ref())
        .is_some_and(|current| Arc::ptr_eq(current, &owner))
    {
        records.remove(&id);
        true
    } else {
        false
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
