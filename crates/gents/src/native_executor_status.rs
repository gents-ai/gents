use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NativeExecutorStatus {
    pub id: u64,
    pub pid: i32,
    pub argv0: String,
    pub tool_name: Option<String>,
    pub started_at: String,
    pub age_ms: i64,
}

pub fn active_native_executors() -> Vec<NativeExecutorStatus> {
    crate::managed_exec::active_executor_snapshots()
        .into_iter()
        .map(|snapshot| NativeExecutorStatus {
            id: snapshot.id,
            pid: snapshot.pid,
            argv0: snapshot.argv0,
            tool_name: snapshot.tool_name,
            started_at: snapshot.started_at,
            age_ms: snapshot.age_ms,
        })
        .collect()
}
