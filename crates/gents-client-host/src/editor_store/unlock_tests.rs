use super::*;
use gents_desktop_core::client::{ClientCore, ClientCoreOptions};
use std::sync::Arc;

enum Cache {
    Missing,
    RejectWrite,
    Store,
    Present(Vec<u8>),
}

async fn open(
    paths: &DesktopPaths,
    interactive: bool,
    cache: Cache,
) -> (Result<ClientCore>, Option<Vec<u8>>) {
    let (native, editor) = tokio::io::duplex(MAX_FRAME_BYTES);
    let provider = Arc::new(Provider::new(paths.root().to_owned(), interactive, native));
    let options = ClientCoreOptions {
        store_key_custody: gents::store_key::StoreKeyCustodyChoice::Keychain,
        store_key_provider: Some(provider),
        ..ClientCoreOptions::local_only()
    };
    let exchange = async {
        let mut editor = frames(editor);
        let Message::Store { record, .. } =
            serde_json::from_slice(&receive(&mut editor).await.unwrap()).unwrap()
        else {
            panic!("expected custody metadata");
        };
        assert!(matches!(
            record.custody,
            StoreKeyCustody::MacosKeychain { .. }
        ));
        let supplied = match &cache {
            Cache::Present(bytes) => Some(bytes.clone()),
            _ => None,
        };
        editor
            .send(Bytes::from(
                serde_json::to_vec(&Reply::Use { bytes: supplied }).unwrap(),
            ))
            .await
            .unwrap();
        if matches!(cache, Cache::Missing | Cache::Present(_)) {
            return None;
        }
        let Message::Key { bytes } =
            serde_json::from_slice(&receive(&mut editor).await.unwrap()).unwrap()
        else {
            panic!("expected the retained native key");
        };
        if matches!(cache, Cache::Store) {
            editor
                .send(Bytes::from(serde_json::to_vec(&Reply::Stored).unwrap()))
                .await
                .unwrap();
        }
        // A failed encrypted-storage write closes the private channel without
        // acknowledging custody; native startup must not open the database.
        Some(bytes)
    };
    tokio::join!(
        ClientCore::start_with_paths_and_options(paths.clone(), options),
        exchange
    )
}

struct CustodyCleanup {
    paths: DesktopPaths,
}

impl Drop for CustodyCleanup {
    fn drop(&mut self) {
        if let Ok(bytes) = std::fs::read(self.paths.store_encryption_path()) {
            let record: StoreEncryption = serde_json::from_slice(&bytes).unwrap();
            record.delete_key(self.paths.store_key_path()).unwrap();
        }
    }
}

/// The child owns its generated credential and disables all system interaction
/// for the whole run. This exercises explicit-unlock selection without approving
/// an ACL or allowing a system dialog in another concurrently running test.
#[test]
fn explicit_unlock_preserves_history_when_the_editor_cannot_cache_its_key() {
    const CHILD: &str = "GENTS_EDITOR_UNLOCK_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "editor_store::unlock_tests::explicit_unlock_preserves_history_when_the_editor_cannot_cache_its_key", "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    gents_server::without_keychain_prompts(|| {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        runtime.block_on(async {
            let home = tempfile::tempdir()?;
            let paths = DesktopPaths::from_root(home.path().to_owned());
            let _cleanup = CustodyCleanup {
                paths: paths.clone(),
            };
            let (created, key) = open(&paths, false, Cache::Store).await;
            let created = Arc::new(created?);
            created.shutdown().await?;
            drop(created);
            let key = key.unwrap();
            let custody = std::fs::read(paths.store_encryption_path())?;
            let identity = std::fs::read(paths.identity_key_path())?;
            let principal = std::fs::read(paths.principal_metadata_path())?;
            let manifest = std::fs::read(paths.node_data_dir().join("MANIFEST"))?;

            let (locked, _) = open(&paths, false, Cache::Missing).await;
            assert!(locked.err().unwrap().is::<HistoryUnlockRequired>());
            let (rejected, offered) = open(&paths, true, Cache::RejectWrite).await;
            assert!(rejected
                .err()
                .unwrap()
                .to_string()
                .contains("editor secret storage disconnected"));
            assert!(
                offered.as_deref() == Some(key.as_slice()),
                "unlock replaced the native key"
            );
            assert!(std::fs::read(paths.node_data_dir().join("MANIFEST"))? == manifest);
            assert!(std::fs::read(paths.identity_key_path())? == identity);
            assert!(std::fs::read(paths.principal_metadata_path())? == principal);
            assert!(std::fs::read(paths.store_encryption_path())? == custody);
            assert!(!paths.store_key_path().exists());

            let (unlocked, offered) = open(&paths, true, Cache::Store).await;
            assert!(
                offered.as_deref() == Some(key.as_slice()),
                "unlock replaced the native key"
            );
            Arc::new(unlocked?).shutdown().await?;
            let (cached, _) = open(&paths, false, Cache::Present(key)).await;
            Arc::new(cached?).shutdown().await?;
            assert!(std::fs::read(paths.identity_key_path())? == identity);
            assert!(std::fs::read(paths.principal_metadata_path())? == principal);
            assert!(std::fs::read(paths.store_encryption_path())? == custody);
            assert!(!paths.store_key_path().exists());
            Ok(())
        })
    })
    .unwrap();
}
