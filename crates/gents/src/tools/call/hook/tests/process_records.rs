use crate::exec::ownership;
use crate::exec::ownership::test_support::OwnedTestProcess;

pub(crate) async fn process_for_generated_outcome(
    registry: &crate::tools::call::hook::BackgroundExecutionRegistry,
    tool_call_id: &str,
    tool_call_doc_id: &str,
    outcome: &str,
) -> Option<OwnedTestProcess> {
    let recorder = registry.process_recorder(tool_call_id, tool_call_doc_id);
    match outcome {
        "stopped" => Some(OwnedTestProcess::spawn(Some(recorder)).await),
        "alreadyExited" => {
            let process = OwnedTestProcess::spawn(Some(recorder)).await;
            let _ = process.identity.stop().await;
            Some(process)
        }
        "notOwned" => None,
        other => panic!("a test-owned group cannot produce process outcome {other}"),
    }
}

#[tokio::test]
async fn call_keyed_process_record_remains_cancellable_after_restart() {
    let directory = tempfile::tempdir().unwrap();
    let registry = crate::tools::call::hook::BackgroundExecutionRegistry::default()
        .with_process_records(directory.path().into());
    let process =
        OwnedTestProcess::spawn(Some(registry.process_recorder("old-call", "physical-tool")))
            .await;
    std::fs::rename(
        crate::exec::ownership::record_path(directory.path(), "physical-tool"),
        crate::exec::ownership::record_path(directory.path(), "old-call"),
    )
    .unwrap();
    let restarted = crate::tools::call::hook::BackgroundExecutionRegistry::default()
        .with_process_records(directory.path().into());
    assert!(restarted.process_record("old-call", "wrong-tool").is_none());
    assert_eq!(
        restarted.stop_execution("old-call", "physical-tool").await,
        crate::exec::ownership::ProcessStopOutcome::Stopped
    );
    restarted.forget_process_record("old-call", "physical-tool");
    assert!(restarted.process_record_list().is_empty());
    process.finish().await;
}

#[cfg(unix)]
#[tokio::test]
async fn durable_process_record_survives_a_fresh_store() {
    use ownership::test_support::OwnedTestProcess;
    use ownership::{ProcessObservation, ProcessRecordStore};

    let dir = tempfile::tempdir().unwrap();
    let registry = crate::tools::call::hook::BackgroundExecutionRegistry::default()
        .with_process_records(dir.path().to_path_buf());
    let process =
        OwnedTestProcess::spawn(Some(registry.process_recorder("spawned:tool", "tool-doc"))).await;
    let restarted = ProcessRecordStore::Durable(dir.path().to_path_buf());
    let record = restarted.read("tool-doc").expect("durable record");
    assert_eq!(record.tool_call_doc_id, "tool-doc");
    assert_eq!(record.identity, process.identity);
    assert_eq!(record.identity.observe(), ProcessObservation::Running);
    assert!(registry
        .process_record("spawned:tool", "other-doc")
        .is_none());
    restarted.remove("tool-doc");
    assert!(restarted.list().is_empty());
    process.finish().await;
}

#[cfg(unix)]
#[tokio::test]
async fn reused_call_ids_keep_distinct_durable_process_owners_after_restart() {
    use ownership::test_support::OwnedTestProcess;
    use ownership::ProcessObservation;
    let dir = tempfile::tempdir().unwrap();
    let registry = crate::tools::call::hook::BackgroundExecutionRegistry::default()
        .with_process_records(dir.path().to_path_buf());
    let first = OwnedTestProcess::spawn(Some(registry.process_recorder("call", "first-doc"))).await;
    let sibling =
        OwnedTestProcess::spawn(Some(registry.process_recorder("call", "sibling-doc"))).await;
    let restarted = crate::tools::call::hook::BackgroundExecutionRegistry::default()
        .with_process_records(dir.path().to_path_buf());
    assert_eq!(restarted.process_record_list().len(), 2);
    assert_eq!(
        restarted.stop_execution("call", "first-doc").await,
        ownership::ProcessStopOutcome::Stopped
    );
    assert_eq!(first.identity.observe(), ProcessObservation::Exited);
    assert_eq!(sibling.identity.observe(), ProcessObservation::Running);
    restarted.forget_process_record("call", "first-doc");
    assert!(restarted.process_record("call", "sibling-doc").is_some());
    assert_eq!(
        restarted.stop_execution("call", "sibling-doc").await,
        ownership::ProcessStopOutcome::Stopped
    );
    first.finish().await;
    sibling.finish().await;
}
