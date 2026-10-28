use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;

pub(crate) trait HostRecord:
    Serialize + serde::de::DeserializeOwned + Clone + Send + 'static
{
    fn key(&self) -> &str;

    fn matches_key(&self, key: &str) -> bool {
        self.key() == key
    }
}

#[derive(Debug, thiserror::Error)]
enum RecordReadError {
    #[error("cannot inspect host records in {}: {source}", .directory.display())]
    Directory {
        directory: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot enumerate host records in {}: {source}", .directory.display())]
    Enumerate {
        directory: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot read host record {}: {source}", .path.display())]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot decode host record {}: {source}", .path.display())]
    Decode {
        path: PathBuf,
        source: serde_json::Error,
    },
}

#[derive(Debug, Clone)]
pub(crate) enum HostRecordStore<R> {
    Volatile(Arc<std::sync::Mutex<rapidhash::RapidHashMap<String, R>>>),
    Durable(PathBuf),
}

impl<R> Default for HostRecordStore<R> {
    fn default() -> Self {
        Self::Volatile(Arc::default())
    }
}

pub(super) fn record_path(dir: &std::path::Path, key: &str) -> PathBuf {
    let name = key
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    dir.join(format!("{name}.json"))
}

impl<R> HostRecordStore<R>
where
    R: HostRecord,
{
    fn volatile(
        records: &std::sync::Mutex<rapidhash::RapidHashMap<String, R>>,
    ) -> std::sync::MutexGuard<'_, rapidhash::RapidHashMap<String, R>> {
        records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    // Sync the record and its directory entry before claiming durable ownership.
    pub(crate) fn write(&self, record: &R) -> std::io::Result<()> {
        let dir = match self {
            Self::Volatile(records) => {
                Self::volatile(records).insert(record.key().to_owned(), record.clone());
                return Ok(());
            }
            Self::Durable(dir) => dir,
        };
        let bytes = serde_json::to_vec(record).map_err(std::io::Error::other)?;
        std::fs::create_dir_all(dir)?;
        let path = record_path(dir, record.key());
        let temporary = path.with_extension("json.tmp");
        {
            use std::io::Write;
            let mut file = std::fs::File::create(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        std::fs::rename(&temporary, &path)?;
        std::fs::File::open(dir).and_then(|dir| dir.sync_all())
    }

    pub(crate) fn read(&self, key: &str) -> Option<R> {
        let dir = match self {
            Self::Volatile(records) => return Self::volatile(records).get(key).cloned(),
            Self::Durable(dir) => dir,
        };
        let bytes = std::fs::read(record_path(dir, key)).ok()?;
        match serde_json::from_slice::<R>(&bytes) {
            Ok(record) if record.matches_key(key) => Some(record),
            Ok(_) | Err(_) => {
                tracing::warn!(key, "ignoring unreadable host record");
                None
            }
        }
    }

    pub(crate) fn remove(&self, key: &str) {
        let dir = match self {
            Self::Volatile(records) => {
                Self::volatile(records).remove(key);
                return;
            }
            Self::Durable(dir) => dir,
        };
        match std::fs::remove_file(record_path(dir, key)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(key, %error, "failed to remove host record");
            }
        }
    }

    pub(crate) fn list(&self) -> Vec<R> {
        let mut records = Vec::new();
        let result = self.visit_records(|record| {
            match record {
                Ok(record) => records.push(record),
                Err(error) => {
                    if matches!(&error, RecordReadError::Enumerate { source, .. }
                        if source.kind() != std::io::ErrorKind::NotFound)
                    {
                        tracing::warn!(%error, "cannot enumerate retained host records");
                    }
                }
            }
            Ok(())
        });
        if let Err(error) = result {
            tracing::warn!(%error, "cannot inspect retained host records");
        }
        records
    }

    pub(crate) fn try_list(&self) -> anyhow::Result<Vec<R>> {
        let mut records = Vec::new();
        self.visit_records(|record| {
            records.push(record?);
            Ok(())
        })?;
        Ok(records)
    }

    fn visit_records<F>(&self, mut visit: F) -> Result<(), RecordReadError>
    where
        F: FnMut(Result<R, RecordReadError>) -> Result<(), RecordReadError>,
    {
        let dir = match self {
            Self::Volatile(records) => {
                for record in Self::volatile(records).values() {
                    visit(Ok(record.clone()))?;
                }
                return Ok(());
            }
            Self::Durable(dir) => dir,
        };
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                return Err(RecordReadError::Directory {
                    directory: dir.clone(),
                    source,
                })
            }
        };
        for entry in entries {
            let path = match entry {
                Ok(entry) => entry.path(),
                Err(source) => {
                    visit(Err(RecordReadError::Enumerate {
                        directory: dir.clone(),
                        source,
                    }))?;
                    continue;
                }
            };
            if !path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                continue;
            }
            let record = std::fs::read(&path)
                .map_err(|source| RecordReadError::Read {
                    path: path.clone(),
                    source,
                })
                .and_then(|bytes| {
                    serde_json::from_slice::<R>(&bytes)
                        .map_err(|source| RecordReadError::Decode { path, source })
                });
            visit(record)?;
        }
        Ok(())
    }
}
