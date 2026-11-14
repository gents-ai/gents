use super::*;
use anyhow::anyhow;
use gents_documents::Task;
use serde_json::json;

fn task(task_id: &str) -> Task {
    serde_json::from_value(json!({
        "task_id": task_id,
        "node_did": "did:key:amy",
        "agent_did": "default",
        "prompt_template": "run"
    }))
    .expect("task")
}

#[test]
fn confirmed_delete_prunes_locally_and_warns_when_refresh_fails() {
    let rows = ClientStoreRows {
        tasks: vec![task("deleted"), task("retained")],
        ..ClientStoreRows::default()
    };
    let store = crate::client::observe::ObservedStores::new().ensure("did:key:amy");
    store.replace_snapshot(ClientStore::from_rows(rows));
    let last_mutation_error = StdRwLock::new(None);

    complete_confirmed_delete(
        store.as_ref(),
        &last_mutation_error,
        Err(anyhow!("replica unavailable")),
        "delete task",
        "config_task_delete",
        "deleted",
        |rows| rows.tasks.retain(|row| row.task_id != "deleted"),
    );

    let snapshot = store.snapshot();
    assert_eq!(snapshot.tasks.len(), 1);
    assert_eq!(snapshot.tasks[0].task_id, "retained");
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
