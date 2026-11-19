use super::*;

#[tokio::test]
async fn an_old_enrollment_completion_cannot_clear_a_replacement() {
    let old = Arc::new(tokio_util::sync::CancellationToken::new());
    let current = Arc::new(tokio_util::sync::CancellationToken::new());
    let mut managed = ManagedServerRuntimeState {
        pairing_cancel: Some(Arc::clone(&current)),
        pairing_task: Some(tauri::async_runtime::spawn(async {})),
        ..Default::default()
    };
    clear_finished_enrollment(&mut managed, &old);
    assert!(Arc::ptr_eq(
        managed.pairing_cancel.as_ref().unwrap(),
        &current
    ));
    assert!(managed.pairing_task.is_some());
    clear_finished_enrollment(&mut managed, &current);
    assert!(managed.pairing_cancel.is_none());
    assert!(managed.pairing_task.is_none());
}

#[tokio::test]
async fn shutdown_cancels_and_drains_an_abandoned_enrollment() {
    let (_temp, state) = orchestration_state();
    let cancel = Arc::new(tokio_util::sync::CancellationToken::new());
    let child = Arc::clone(&cancel);
    let (finished, observed) = tokio::sync::oneshot::channel();
    let task = tauri::async_runtime::spawn(async move {
        child.cancelled().await;
        let _ = finished.send(());
    });
    {
        let mut managed = state.managed_server.lock().await;
        managed.pairing_cancel = Some(Arc::clone(&cancel));
        managed.pairing_task = Some(task);
    }
    drain_managed_runtime_pairing(&state).await;
    assert!(cancel.is_cancelled());
    observed.await.unwrap();
    let managed = state.managed_server.lock().await;
    assert!(managed.pairing_cancel.is_none());
    assert!(managed.pairing_task.is_none());
}
