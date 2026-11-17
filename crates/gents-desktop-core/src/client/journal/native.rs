use super::*;
use gents_store::local::store_key::StoreKeyCustodyChoice;

#[tokio::test]
async fn journal_reopens_with_the_same_user_and_only_journal_collections() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let paths = DesktopPaths::from_root(directory.path());
    let options = JournalOptions {
        store_key_custody: StoreKeyCustodyChoice::File,
        ..Default::default()
    };
    let journal = ClientJournal::open(paths.clone(), options.clone()).await?;
    let client = journal.identity().did().to_owned();
    assert!(ClientJournal::open(paths.clone(), options.clone())
        .await
        .is_err());
    let schemas = journal.node().get_all_collection_versions().await?;
    assert!(!schemas.is_empty());
    assert!(schemas.iter().all(|collection| collection
        .policy
        .as_ref()
        .is_some_and(|policy| policy.resource_name
            == gents_protocol::schemas::CLIENT_JOURNAL_POLICY_RESOURCE)));
    let released = journal.store_release();
    journal.shutdown().await?;
    assert!(matches!(
        gents_store::local::home::try_store(paths.root(), paths.node_data_dir())?,
        gents_store::local::home::StoreAttempt::Wait(_)
    ));
    drop(journal);
    released.released().await;
    let reopened = ClientJournal::open(paths, options).await?;
    assert_eq!(reopened.identity().did(), client);
    reopened.shutdown().await?;
    Ok(())
}
