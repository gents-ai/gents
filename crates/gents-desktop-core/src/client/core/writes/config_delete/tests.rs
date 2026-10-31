use super::*;
use anyhow::anyhow;
use gents::document_config::Task;
use serde_json::json;

fn task(task_id: &str) -> Task {
    serde_json::from_value(json!({
        "task_id": task_id,
        "node_did": "did:key:amy",
        "agent_id": "default",
        "prompt_template": "run"
    }))
    .expect("task")
}

#[test]
fn confirmed_delete_prunes_locally_and_warns_when_refresh_fails() {
    let rows = ClientStoreRows {
        tasks: vec![task("deleted"), task("retained")],
        task_source_node_dids: vec![None, Some("did:key:remote".to_string())],
        ..ClientStoreRows::default()
    };
    let (store, _version_rx) = ObservedStore::new(ClientStore::from_rows(rows));
    let last_mutation_error = StdRwLock::new(None);

    complete_confirmed_delete(
        store.as_ref(),
        &last_mutation_error,
        Err(anyhow!("replica unavailable")),
        "delete task",
        "config_task_delete",
        "deleted",
        |rows| {
            retain_rows_with_sources(&mut rows.tasks, &mut rows.task_source_node_dids, |row| {
                row.task_id != "deleted"
            });
        },
    );

    let snapshot = store.snapshot();
    assert_eq!(snapshot.tasks.len(), 1);
    assert_eq!(snapshot.tasks[0].task_id, "retained");
    assert_eq!(
        snapshot.task_source_node_dids,
        vec![Some("did:key:remote".to_string())]
    );
    assert_eq!(
        last_mutation_error
            .read()
            .expect("mutation error lock poisoned")
            .as_deref(),
        Some(
            "delete task succeeded, but refreshing the source snapshot failed: replica unavailable"
        )
    );
}
