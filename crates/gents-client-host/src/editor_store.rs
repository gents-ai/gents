use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use gents::store_key::{StoreEncryption, StoreKey, StoreKeyCustody, StoreKeyUnavailable};
use gents_desktop_bridge::{BridgeError, BridgeErrorCode};
use gents_desktop_core::client::{ClientStoreKeyProvider, DesktopPaths};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Mutex;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

const MAX_FRAME_BYTES: usize = 4096;
pub use gents_desktop_bridge::host::protocol::EDITOR_STORE_BOOTSTRAP_VERSION as VERSION;

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Message {
    Store {
        version: u32,
        home: String,
        record: StoreEncryption,
    },
    Key {
        bytes: Vec<u8>,
    },
    Ready,
    Error {
        error: BridgeError,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
enum Reply {
    Use { bytes: Option<Vec<u8>> },
    Stored,
}

pub fn frames<S>(stream: S) -> Framed<S, LengthDelimitedCodec>
where
    S: AsyncRead + AsyncWrite,
{
    LengthDelimitedCodec::builder()
        .max_frame_length(MAX_FRAME_BYTES)
        .new_framed(stream)
}

pub async fn send<S>(channel: &mut Framed<S, LengthDelimitedCodec>, message: &Message) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    channel
        .send(Bytes::from(serde_json::to_vec(message)?))
        .await?;
    Ok(())
}

pub async fn receive<S>(channel: &mut Framed<S, LengthDelimitedCodec>) -> Result<bytes::BytesMut>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    tokio::time::timeout(std::time::Duration::from_secs(30), channel.next())
        .await
        .context("editor secret storage did not respond")?
        .context("editor secret storage disconnected")?
        .context("invalid editor secret storage frame")
}

#[derive(Debug)]
struct HistoryUnlockRequired;

impl std::fmt::Display for HistoryUnlockRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Unlock local history in the editor to save access for future launches")
    }
}

impl std::error::Error for HistoryUnlockRequired {}

/// Only the extension host receives this private bootstrap channel. It is
/// retired before ordinary client framing starts; keys never become host RPCs,
/// application events, command arguments, environment variables, or log fields.
pub struct Provider<S> {
    home: PathBuf,
    interactive: bool,
    channel: Mutex<Framed<S, LengthDelimitedCodec>>,
}

impl<S> std::fmt::Debug for Provider<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EditorStoreKeyProvider")
    }
}

impl<S> Provider<S>
where
    S: AsyncRead + AsyncWrite,
{
    pub fn new(home: PathBuf, interactive: bool, channel: S) -> Self {
        Self {
            home,
            interactive,
            channel: Mutex::new(frames(channel)),
        }
    }
}

impl<S> Provider<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    pub async fn ready(&self) -> Result<()> {
        send(&mut *self.channel.lock().await, &Message::Ready).await
    }

    pub async fn failed(&self, error: &anyhow::Error) -> Result<()> {
        let (code, message) = if error.downcast_ref::<HistoryUnlockRequired>().is_some()
            || error.downcast_ref::<StoreKeyUnavailable>().is_some()
        {
            (
                    BridgeErrorCode::LocalHistoryLocked,
                    "Unlock local history to cache its encryption key in VS Code. Your chats are unchanged.",
                )
        } else if gents::storage_backend::incompatible_store(
            error,
            DesktopPaths::from_root(&self.home).node_data_dir(),
        )
        .is_some()
        {
            (
                    BridgeErrorCode::IncompatibleLocalStore,
                    "This local history cannot be opened by this Gents client. Its store and encryption key are unchanged.",
                )
        } else {
            (
                BridgeErrorCode::ClientStartFailed,
                "Local history could not be opened. Its store and encryption key are unchanged.",
            )
        };
        send(
            &mut *self.channel.lock().await,
            &Message::Error {
                error: BridgeError::new(code, message),
            },
        )
        .await
    }
}

#[async_trait::async_trait]
impl<S> ClientStoreKeyProvider for Provider<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    async fn load(
        &self,
        record: &StoreEncryption,
        key_file: &Path,
        data_path: &Path,
    ) -> Result<StoreKey> {
        anyhow::ensure!(
            record.version == gents::store_key::STORE_ENCRYPTION_VERSION,
            "unsupported editor store encryption version"
        );
        if matches!(record.custody, StoreKeyCustody::File) {
            return record.initialize(key_file, data_path);
        }
        let mut channel = self.channel.lock().await;
        send(
            &mut channel,
            &Message::Store {
                version: VERSION,
                home: self.home.canonicalize()?.to_string_lossy().into_owned(),
                record: record.clone(),
            },
        )
        .await?;
        let reply: Reply = serde_json::from_slice(&receive(&mut channel).await?)
            .context("invalid editor secret storage response")?;
        match reply {
            Reply::Use { bytes: Some(bytes) } => return StoreKey::from_host(&bytes),
            Reply::Use { bytes: None } => {}
            Reply::Stored => bail!("editor acknowledged a key before receiving it"),
        }
        // Background startup must never call Keychain: an updated ad hoc signed
        // helper can trigger system consent even when the login keychain is open.
        if !self.interactive {
            return Err(HistoryUnlockRequired.into());
        }
        let record = record.clone();
        let key_file = key_file.to_owned();
        let data_path = data_path.to_owned();
        let key = tokio::task::spawn_blocking(move || record.initialize(&key_file, &data_path))
            .await
            .context("loading the editor store key")??;
        let message = key.with_secret(|bytes| Message::Key {
            bytes: bytes.to_vec(),
        });
        send(&mut channel, &message).await?;
        let reply: Reply = serde_json::from_slice(&receive(&mut channel).await?)
            .context("invalid editor secret storage acknowledgement")?;
        anyhow::ensure!(
            matches!(reply, Reply::Stored),
            "editor did not retain the store key"
        );
        Ok(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn read_message<S>(channel: &mut Framed<S, LengthDelimitedCodec>) -> Message
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        serde_json::from_slice(&receive(channel).await.unwrap()).unwrap()
    }

    async fn reply<S>(channel: &mut Framed<S, LengthDelimitedCodec>, reply: Reply)
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        channel
            .send(Bytes::from(serde_json::to_vec(&reply).unwrap()))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_cached_key_avoids_the_native_keychain_entirely() {
        let home = tempfile::tempdir().unwrap();
        let record = StoreEncryption {
            version: 1,
            custody: StoreKeyCustody::MacosKeychain {
                keychain_label: "nonexistent-test-item".into(),
            },
        };
        let key_file = home.path().join("store.key");
        let data = home.path().join("data");
        let (native, editor) = tokio::io::duplex(MAX_FRAME_BYTES);
        let provider = Provider::new(home.path().to_owned(), false, native);
        let mut editor = frames(editor);
        let load = tokio::spawn(async move { provider.load(&record, &key_file, &data).await });
        assert!(matches!(
            read_message(&mut editor).await,
            Message::Store { .. }
        ));
        reply(
            &mut editor,
            Reply::Use {
                bytes: Some(vec![17; 32]),
            },
        )
        .await;
        let key = load.await.unwrap().unwrap();
        key.with_secret(|bytes| assert_eq!(bytes, &[17; 32]));
    }

    #[tokio::test]
    async fn a_cache_miss_never_queries_or_creates_a_native_key() {
        let home = tempfile::tempdir().unwrap();
        let record = StoreEncryption {
            version: 1,
            custody: StoreKeyCustody::MacosKeychain {
                keychain_label: "nonexistent-test-item".into(),
            },
        };
        let key_file = home.path().join("store.key");
        let data = home.path().join("data");
        let (native, editor) = tokio::io::duplex(MAX_FRAME_BYTES);
        let provider = Provider::new(home.path().to_owned(), false, native);
        let mut editor = frames(editor);
        let load = tokio::spawn(async move { provider.load(&record, &key_file, &data).await });
        assert!(matches!(
            read_message(&mut editor).await,
            Message::Store { .. }
        ));
        reply(&mut editor, Reply::Use { bytes: None }).await;
        assert!(load
            .await
            .unwrap()
            .unwrap_err()
            .downcast_ref::<HistoryUnlockRequired>()
            .is_some());
    }

    #[tokio::test]
    async fn file_custody_remains_durable_without_an_editor_cache() {
        let home = tempfile::tempdir().unwrap();
        let record = StoreEncryption {
            version: 1,
            custody: StoreKeyCustody::File,
        };
        let key_file = home.path().join("store.key");
        let data = home.path().join("data");
        let (native, _editor) = tokio::io::duplex(MAX_FRAME_BYTES);
        let provider = Provider::new(home.path().to_owned(), false, native);
        let first = provider.load(&record, &key_file, &data).await.unwrap();
        let second = provider.load(&record, &key_file, &data).await.unwrap();
        first.with_secret(|bytes| second.with_secret(|next| assert_eq!(bytes, next)));
        assert!(key_file.is_file());
    }

    #[tokio::test]
    async fn unavailable_native_keys_require_an_explicit_unlock() {
        let home = tempfile::tempdir().unwrap();
        let (native, editor) = tokio::io::duplex(MAX_FRAME_BYTES);
        let provider = Provider::new(home.path().to_owned(), false, native);
        provider
            .failed(&HistoryUnlockRequired.into())
            .await
            .unwrap();
        assert!(matches!(
            read_message(&mut frames(editor)).await,
            Message::Error {
                error: BridgeError {
                    code: BridgeErrorCode::LocalHistoryLocked,
                    retryable: false,
                    ..
                }
            }
        ));
    }

    #[tokio::test]
    async fn incompatible_store_failures_retain_their_class_without_raw_causes() {
        let home = tempfile::tempdir().unwrap();
        let (native, editor) = tokio::io::duplex(MAX_FRAME_BYTES);
        let provider = Provider::new(home.path().to_owned(), false, native);
        let failure = anyhow::Error::from(gents::storage_backend::IncompatibleStore {
            kind: gents::storage_backend::IncompatibleStoreKind::ForeignVersion,
            data_path: PathBuf::from("/private/retained-history"),
        })
        .context("sensitive native cause");
        provider.failed(&failure).await.unwrap();
        let Message::Error { error } = read_message(&mut frames(editor)).await else {
            panic!("expected a typed startup failure");
        };
        assert_eq!(error.code, BridgeErrorCode::IncompatibleLocalStore);
        assert!(!error.retryable);
        assert!(error.endpoint.is_none());
        assert!(!error.message.contains("/private/retained-history"));
        assert!(!error.message.contains("sensitive native cause"));
    }

    #[tokio::test]
    async fn unknown_startup_failures_remain_retryable_without_raw_causes() {
        let home = tempfile::tempdir().unwrap();
        let (native, editor) = tokio::io::duplex(MAX_FRAME_BYTES);
        let provider = Provider::new(home.path().to_owned(), false, native);
        provider
            .failed(&anyhow::anyhow!("sensitive native cause"))
            .await
            .unwrap();
        let Message::Error { error } = read_message(&mut frames(editor)).await else {
            panic!("expected a typed startup failure");
        };
        assert_eq!(error.code, BridgeErrorCode::ClientStartFailed);
        assert!(error.retryable);
        assert!(!error.message.contains("sensitive native cause"));
    }

    #[tokio::test]
    async fn missing_editor_keys_never_replace_the_key_for_an_existing_store() {
        let home = tempfile::tempdir().unwrap();
        let record = StoreEncryption {
            version: 1,
            custody: StoreKeyCustody::File,
        };
        let data = home.path().join("data");
        std::fs::create_dir(&data).unwrap();
        std::fs::write(data.join("MANIFEST"), b"existing encrypted store").unwrap();
        let key_file = home.path().join("store.key");
        let (native, editor) = tokio::io::duplex(MAX_FRAME_BYTES);
        let provider = Provider::new(home.path().to_owned(), false, native);
        let observed_data = data.clone();
        let _editor = editor;
        let load = tokio::spawn(async move { provider.load(&record, &key_file, &data).await });
        assert!(load.await.unwrap().is_err());
        assert_eq!(
            std::fs::read(observed_data.join("MANIFEST")).unwrap(),
            b"existing encrypted store"
        );
    }
}
