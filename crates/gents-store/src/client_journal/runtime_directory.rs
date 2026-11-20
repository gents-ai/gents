use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::Arc,
};

use gents_protocol::runtime_connection::RuntimeConnection;
use serde::{Deserialize, Serialize};
use tokio::sync::{watch, Mutex};

#[derive(Debug, thiserror::Error)]
pub enum DirectoryError {
    #[error("the configured runtime does not have a valid Node and GraphQL binding")]
    Binding,
    #[error("this client already has a connection for the same runtime")]
    Duplicate,
    #[error("this client directory already has a writer")]
    Busy,
    #[error("accessing the client runtime directory")]
    Io(#[from] std::io::Error),
    #[error("decoding the client runtime directory")]
    Json(#[from] serde_json::Error),
    #[error("the client runtime directory operation ended unexpectedly")]
    Task(#[from] tokio::task::JoinError),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    connections: Vec<RuntimeConnection>,
}

struct Writer {
    path: PathBuf,
    _lease: File,
}

#[derive(Clone)]
pub struct RuntimeDirectory {
    writer: Arc<Mutex<Writer>>,
    current: watch::Sender<Vec<RuntimeConnection>>,
}

pub async fn load_runtime_connections(
    path: &Path,
) -> Result<Vec<RuntimeConnection>, DirectoryError> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || read(&path)).await?
}

fn read(path: &Path) -> Result<Vec<RuntimeConnection>, DirectoryError> {
    let data = match std::fs::read(path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let stored: Stored = serde_json::from_slice(&data)?;
    for (index, record) in stored.connections.iter().enumerate() {
        if !record.validate() {
            return Err(DirectoryError::Binding);
        }
        if stored.connections[..index]
            .iter()
            .any(|other| other.node_did == record.node_did)
        {
            return Err(DirectoryError::Duplicate);
        }
    }
    Ok(stored.connections)
}

impl RuntimeDirectory {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, DirectoryError> {
        let path = path.as_ref().to_owned();
        let (writer, records) = tokio::task::spawn_blocking(move || {
            let parent = path.parent().ok_or(DirectoryError::Binding)?;
            std::fs::create_dir_all(parent)?;
            let lease = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(path.with_extension("lock"))?;
            match lease.try_lock() {
                Ok(()) => {}
                Err(std::fs::TryLockError::WouldBlock) => return Err(DirectoryError::Busy),
                Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
            }
            let records = read(&path)?;
            Ok::<_, DirectoryError>((
                Writer {
                    path,
                    _lease: lease,
                },
                records,
            ))
        })
        .await??;
        let (current, _) = watch::channel(records);
        Ok(Self {
            writer: Arc::new(Mutex::new(writer)),
            current,
        })
    }

    pub fn records(&self) -> Vec<RuntimeConnection> {
        self.current.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<Vec<RuntimeConnection>> {
        self.current.subscribe()
    }

    pub async fn upsert(&self, record: RuntimeConnection) -> Result<(), DirectoryError> {
        if !record.validate() {
            return Err(DirectoryError::Binding);
        }
        self.edit(move |records| {
            match records
                .iter()
                .position(|other| other.node_did == record.node_did)
            {
                Some(index) => records[index] = record,
                None => records.push(record),
            }
            true
        })
        .await?;
        Ok(())
    }

    pub async fn remove(&self, node: &str) -> Result<bool, DirectoryError> {
        let node = node.to_owned();
        self.edit(move |records| {
            let before = records.len();
            records.retain(|record| record.node_did != node);
            before != records.len()
        })
        .await
    }

    pub async fn retire_local_home(
        &self,
        home: &Path,
        current_node: Option<&str>,
    ) -> Result<usize, DirectoryError> {
        let stale = self
            .records()
            .into_iter()
            .filter(|connection| {
                connection
                    .local_node_home
                    .as_deref()
                    .is_some_and(|local| Path::new(local) == home)
                    && current_node != Some(connection.node_did.as_str())
            })
            .collect::<Vec<_>>();
        let mut retired = 0;
        for connection in stale {
            retired += usize::from(self.remove(&connection.node_did).await?);
        }
        Ok(retired)
    }

    async fn edit<F>(&self, edit: F) -> Result<bool, DirectoryError>
    where
        F: FnOnce(&mut Vec<RuntimeConnection>) -> bool + Send + 'static,
    {
        let writer = self.writer.clone().lock_owned().await;
        let current = self.current.clone();
        tokio::task::spawn_blocking(move || {
            let mut records = current.borrow().clone();
            if !edit(&mut records) {
                return Ok(false);
            }
            let parent = writer.path.parent().ok_or(DirectoryError::Binding)?;
            let mut file = tempfile::NamedTempFile::new_in(parent)?;
            serde_json::to_writer(
                file.as_file_mut(),
                &Stored {
                    connections: records.clone(),
                },
            )?;
            file.as_file().sync_all()?;
            file.persist(&writer.path).map_err(|error| error.error)?;
            current.send_replace(records);
            #[cfg(unix)]
            File::open(parent)?.sync_all()?;
            Ok::<_, DirectoryError>(true)
        })
        .await?
    }
}
