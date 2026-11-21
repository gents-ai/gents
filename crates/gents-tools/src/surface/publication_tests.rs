use super::*;

#[tokio::test(start_paused = true)]
async fn the_wait_is_bounded_and_keeps_the_busy_cause() {
    let mut attempts = 0;
    let error = wait(Instant::now() + Duration::from_millis(200), || {
        attempts += 1;
        Err::<(), _>(PublishedPluginReadError::Busy.into())
    })
    .await
    .unwrap_err();
    assert_eq!(attempts, 3);
    assert!(PublishedPluginReadError::error_chain_is_busy(
        error.as_ref()
    ));
    assert!(format!("{error:#}").contains("wait exhausted after 2 retries"));
}

#[tokio::test(start_paused = true)]
async fn a_permanent_failure_is_not_retried() {
    let mut attempts = 0;
    wait(Instant::now() + PUBLICATION_WAIT, || {
        attempts += 1;
        Err::<(), _>(anyhow::anyhow!("pinned digest does not match"))
    })
    .await
    .unwrap_err();
    assert_eq!(attempts, 1);
}

#[tokio::test(start_paused = true)]
async fn construction_resumes_once_the_installer_releases() {
    let mut attempts = 0;
    let built = wait(Instant::now() + PUBLICATION_WAIT, || {
        attempts += 1;
        if attempts == 1 {
            Err(PublishedPluginReadError::Busy.into())
        } else {
            Ok("the pinned tool")
        }
    })
    .await
    .unwrap();
    assert_eq!(attempts, 2);
    assert_eq!(built, "the pinned tool");
}

#[tokio::test]
async fn the_guard_waits_for_an_installer_and_never_adopts_a_replaced_home() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    gents_pack::plugin::store::bootstrap_store(&home).unwrap();
    let pin = StoreReadPin::capture(&home);
    let writer = gents_pack::plugin::store::lock_store(&home).unwrap();
    let waiting = tokio::spawn({
        let pin = pin.clone();
        async move { publication_guard_until(Some(&pin), Instant::now() + PUBLICATION_WAIT).await }
    });
    tokio::task::yield_now().await;
    drop(writer);
    assert!(waiting.await.unwrap().unwrap().is_some());

    std::fs::rename(&home, root.path().join("retired")).unwrap();
    std::fs::create_dir(&home).unwrap();
    let error = publication_guard_until(Some(&pin), Instant::now() + PUBLICATION_WAIT)
        .await
        .err()
        .expect("a replaced home is never adopted");
    assert!(
        matches!(error, PublishedPluginReadError::Absent(_)),
        "{error:?}"
    );
    assert_eq!(std::fs::read_dir(&home).unwrap().count(), 0);
}
