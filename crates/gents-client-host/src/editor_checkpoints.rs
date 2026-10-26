use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, ensure, Context, Result};
use gents::file_lock::FileLock;
use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;
const MAX_BYTES: usize = gents_desktop_bridge::host::protocol::MAX_FRAME_BYTES;
const JOURNAL: &str = "editor-undo.v1.json";
const EDITOR_UNDO_LOCK: &str = ".editor-undo.lock";

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u32,
    ledger: Vec<Operation>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Operation {
    id: String,
    sequence: Sequence,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Sequence {
    items: Vec<Progress>,
    closed: bool,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Progress {
    reference: String,
    actions: Vec<String>,
    uri: String,
    restore: Restore,
    save: Save,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum Restore {
    Fresh,
    Pending,
    Applied,
    Rejected,
    Unknown,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Save {
    state: SaveState,
    exact: bool,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum SaveState {
    Pending,
    Saving,
    Saved,
    Conflict,
    Failed,
    Unknown,
}

impl Envelope {
    fn validate(&self) -> Result<()> {
        ensure!(self.version == VERSION, "unsupported checkpoint version");
        ensure!(self.ledger.len() <= 4096, "checkpoint ledger is too large");
        let bounded = |value: &str| value.len() <= 64 * 1024;
        for operation in &self.ledger {
            ensure!(bounded(&operation.id), "checkpoint reference is too large");
            ensure!(
                operation.sequence.items.len() <= 64,
                "checkpoint group is too large"
            );
            for progress in &operation.sequence.items {
                ensure!(
                    bounded(&progress.reference) && bounded(&progress.uri),
                    "checkpoint reference is too large"
                );
                ensure!(
                    progress.actions.len() <= 4096
                        && progress.actions.iter().all(|value| bounded(value)),
                    "checkpoint actions are too large"
                );
            }
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(tag = "command", rename_all = "camelCase", deny_unknown_fields)]
enum Request {
    Read {
        version: u32,
        id: u64,
    },
    Write {
        version: u32,
        id: u64,
        ledger: Vec<Operation>,
    },
    Close {
        version: u32,
        id: u64,
    },
}

impl Request {
    fn header(&self) -> (u32, u64) {
        match self {
            Self::Read { version, id }
            | Self::Write { version, id, .. }
            | Self::Close { version, id } => (*version, *id),
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum Reply<'a> {
    Acquired {
        version: u32,
    },
    Busy {
        version: u32,
    },
    Error {
        version: u32,
        id: Option<u64>,
    },
    Absent {
        version: u32,
        id: u64,
    },
    Loaded {
        version: u32,
        id: u64,
        ledger: &'a [Operation],
    },
    Durable {
        version: u32,
        id: u64,
    },
    Closed {
        version: u32,
        id: u64,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Staged,
    FileSynced,
    Installed,
    DirectorySynced,
}

struct Session {
    directory: PathBuf,
    _guard: FileLock,
    loaded: bool,
    poisoned: bool,
}

fn regular(metadata: &std::fs::Metadata) -> Result<()> {
    ensure!(metadata.is_file(), "checkpoint must be a regular file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(metadata.nlink() == 1, "checkpoint must have one link");
        ensure!(
            metadata.uid() == unsafe { libc::geteuid() },
            "checkpoint owner differs"
        );
        ensure!(
            metadata.mode() & 0o077 == 0,
            "checkpoint permissions are not private"
        );
    }
    Ok(())
}

fn open_regular(path: &Path, create: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(create)
        .create(create)
        .truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    regular(&file.metadata()?)?;
    Ok(file)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    bail!("durable checkpoint directory sync is unavailable on this platform")
}

impl Session {
    /// This directory belongs to the editor's workspace-private storage. Its
    /// fixed filenames are not source paths. The stable lock inode is never
    /// replaced; holding it does not authorize an editor or source mutation.
    fn open(directory: &Path) -> Result<Option<Self>> {
        ensure!(
            directory.is_absolute(),
            "checkpoint directory must be absolute"
        );
        #[cfg(not(unix))]
        bail!("durable checkpoints are unavailable on this platform");
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, MetadataExt};
            match std::fs::DirBuilder::new().mode(0o700).create(directory) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
            let metadata = std::fs::symlink_metadata(directory)?;
            ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "checkpoint directory is not a directory"
            );
            ensure!(
                metadata.uid() == unsafe { libc::geteuid() } && metadata.mode() & 0o077 == 0,
                "checkpoint directory is not private"
            );
            // Another helper may have just created this directory. Every
            // acquisition must persist its parent entry before readiness.
            sync_directory(
                directory
                    .parent()
                    .context("checkpoint directory has no parent")?,
            )?;
            let file = open_regular(&directory.join(EDITOR_UNDO_LOCK), true)?;
            let guard = match FileLock::try_exclusive(file) {
                Ok(guard) => guard,
                Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
                Err(error) => return Err(error.into()),
            };
            sync_directory(directory)?;
            Ok(Some(Self {
                directory: directory.into(),
                _guard: guard,
                loaded: false,
                poisoned: false,
            }))
        }
    }

    fn read(&mut self) -> Result<Option<Envelope>> {
        ensure!(
            !self.loaded && !self.poisoned,
            "checkpoint session cannot reload"
        );
        let result = (|| {
            let file = match open_regular(&self.directory.join(JOURNAL), false) {
                Ok(file) => file,
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
                {
                    return Ok(None);
                }
                Err(error) => return Err(error),
            };
            ensure!(
                file.metadata()?.len() <= MAX_BYTES as u64,
                "checkpoint is too large"
            );
            let mut bytes = Vec::new();
            file.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
            ensure!(bytes.len() <= MAX_BYTES, "checkpoint is too large");
            let envelope: Envelope = serde_json::from_slice(&bytes)?;
            envelope.validate()?;
            Ok(Some(envelope))
        })();
        self.loaded = result.is_ok();
        self.poisoned = result.is_err();
        result
    }

    fn write(&mut self, ledger: Vec<Operation>) -> Result<()> {
        self.write_with(ledger, |_| Ok(()))
    }

    /// Acknowledgement follows file sync, atomic replacement and real directory
    /// sync. Unsupported or uncertain durability poisons this session without a
    /// retry. A missing reply is not success even if replacement completed.
    fn write_with(
        &mut self,
        ledger: Vec<Operation>,
        mut observe: impl FnMut(Stage) -> Result<()>,
    ) -> Result<()> {
        ensure!(
            self.loaded && !self.poisoned,
            "checkpoint session is unavailable"
        );
        let result = (|| {
            let envelope = Envelope {
                version: VERSION,
                ledger,
            };
            envelope.validate()?;
            let bytes = serde_json::to_vec(&envelope)?;
            ensure!(bytes.len() <= MAX_BYTES, "checkpoint is too large");
            let mut staged = tempfile::NamedTempFile::new_in(&self.directory)?;
            staged.write_all(&bytes)?;
            observe(Stage::Staged)?;
            staged.as_file().sync_all()?;
            observe(Stage::FileSynced)?;
            staged.persist(self.directory.join(JOURNAL))?;
            observe(Stage::Installed)?;
            sync_directory(&self.directory)?;
            observe(Stage::DirectorySynced)?;
            Ok(())
        })();
        self.poisoned = result.is_err();
        result
    }
}

fn receive(input: &mut impl Read) -> Result<Option<Request>> {
    let mut header = [0; 4];
    match input.read(&mut header[..1])? {
        0 => return Ok(None),
        1 => {}
        _ => unreachable!(),
    }
    input.read_exact(&mut header[1..])?;
    let length = u32::from_be_bytes(header) as usize;
    ensure!(length <= MAX_BYTES, "checkpoint frame is too large");
    let mut bytes = vec![0; length];
    input.read_exact(&mut bytes)?;
    Ok(Some(serde_json::from_slice(&bytes)?))
}

fn send(output: &mut impl Write, reply: &Reply<'_>) -> Result<()> {
    let bytes = serde_json::to_vec(reply)?;
    ensure!(bytes.len() <= MAX_BYTES, "checkpoint reply is too large");
    output.write_all(&(bytes.len() as u32).to_be_bytes())?;
    output.write_all(&bytes)?;
    output.flush()?;
    Ok(())
}

/// This private process owns checkpoint IO only. It bypasses client discovery,
/// identity and credentials. The editor remains the sole effect-policy owner;
/// losing this process releases the lock and requires the editor to stop all
/// new launches, preserving the durable unresolved workspace barrier.
pub fn serve(directory: &Path, mut input: impl Read, mut output: impl Write) -> Result<()> {
    let mut session = match Session::open(directory) {
        Ok(Some(session)) => session,
        Ok(None) => return send(&mut output, &Reply::Busy { version: VERSION }),
        Err(_) => {
            send(
                &mut output,
                &Reply::Error {
                    version: VERSION,
                    id: None,
                },
            )?;
            bail!("checkpoint storage is unavailable");
        }
    };
    send(&mut output, &Reply::Acquired { version: VERSION })?;
    let mut last_id = 0;
    loop {
        let request = match receive(&mut input) {
            Ok(Some(request)) => request,
            Ok(None) => return Ok(()),
            Err(_) => {
                send(
                    &mut output,
                    &Reply::Error {
                        version: VERSION,
                        id: None,
                    },
                )?;
                bail!("checkpoint input is invalid");
            }
        };
        let (version, id) = request.header();
        if version != VERSION || id <= last_id || id > 9_007_199_254_740_991 {
            send(
                &mut output,
                &Reply::Error {
                    version: VERSION,
                    id: Some(id),
                },
            )?;
            bail!("checkpoint request is invalid");
        }
        last_id = id;
        let result = match request {
            Request::Read { .. } => match session.read() {
                Ok(None) => send(
                    &mut output,
                    &Reply::Absent {
                        version: VERSION,
                        id,
                    },
                ),
                Ok(Some(envelope)) => send(
                    &mut output,
                    &Reply::Loaded {
                        version: VERSION,
                        id,
                        ledger: &envelope.ledger,
                    },
                ),
                Err(error) => Err(error),
            },
            Request::Write { ledger, .. } => session.write(ledger).and_then(|()| {
                send(
                    &mut output,
                    &Reply::Durable {
                        version: VERSION,
                        id,
                    },
                )
            }),
            Request::Close { .. } => {
                return send(
                    &mut output,
                    &Reply::Closed {
                        version: VERSION,
                        id,
                    },
                );
            }
        };
        if result.is_err() {
            send(
                &mut output,
                &Reply::Error {
                    version: VERSION,
                    id: Some(id),
                },
            )?;
            bail!("checkpoint operation could not be acknowledged");
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::json;

    fn ledger(id: &str) -> Vec<Operation> {
        serde_json::from_value(json!([{"id":id,"sequence":{"items":[{
            "reference":"retained-reference","actions":["physical-action"],"uri":"file:///project/file",
            "restore":"pending","save":{"state":"pending","exact":false}}],"closed":false}}])).unwrap()
    }

    #[test]
    fn checkpoint_faults_poison_without_retry_and_preserve_exact_old_or_new_record() {
        for stage in [
            Stage::Staged,
            Stage::FileSynced,
            Stage::Installed,
            Stage::DirectorySynced,
        ] {
            let root = tempfile::tempdir().unwrap();
            let directory = root.path().join("checkpoints");
            let mut session = Session::open(&directory).unwrap().unwrap();
            assert_eq!(session.read().unwrap(), None);
            session.write(ledger("old")).unwrap();
            assert!(session
                .write_with(ledger("new"), |current| {
                    if current == stage {
                        bail!("injected checkpoint IO boundary failure")
                    }
                    Ok(())
                })
                .is_err());
            assert!(session.write(ledger("not-retried")).is_err());
            assert!(Session::open(&directory).unwrap().is_none());
            drop(session);
            let mut reopened = Session::open(&directory).unwrap().unwrap();
            let retained = reopened.read().unwrap().unwrap();
            let expected = if matches!(stage, Stage::Installed | Stage::DirectorySynced) {
                "new"
            } else {
                "old"
            };
            assert_eq!(retained.ledger, ledger(expected));
        }
    }

    #[test]
    fn checkpoint_corruption_and_non_reference_fields_refuse_without_replacement() {
        let valid = serde_json::to_value(Envelope {
            version: VERSION,
            ledger: ledger("old"),
        })
        .unwrap();
        let mut extra = valid.clone();
        extra["ledger"][0]["sequence"]["items"][0]["before"] = json!("source bytes are forbidden");
        let mut save_extra = valid.clone();
        save_extra["ledger"][0]["sequence"]["items"][0]["save"]["grant"] = json!(true);
        let mut future = valid.clone();
        future["version"] = json!(2);
        let mut wrong_type = valid.clone();
        wrong_type["ledger"][0]["sequence"]["closed"] = json!("false");
        for bytes in [
            b"{".to_vec(),
            vec![],
            serde_json::to_vec(&extra).unwrap(),
            serde_json::to_vec(&save_extra).unwrap(),
            serde_json::to_vec(&future).unwrap(),
            serde_json::to_vec(&wrong_type).unwrap(),
            vec![b' '; MAX_BYTES + 1],
        ] {
            let root = tempfile::tempdir().unwrap();
            let directory = root.path().join("checkpoints");
            let mut session = Session::open(&directory).unwrap().unwrap();
            std::fs::write(directory.join(JOURNAL), &bytes).unwrap();
            std::fs::set_permissions(
                directory.join(JOURNAL),
                std::os::unix::fs::PermissionsExt::from_mode(0o600),
            )
            .unwrap();
            assert!(session.read().is_err());
            assert!(session.write(ledger("replacement")).is_err());
            assert_eq!(std::fs::read(directory.join(JOURNAL)).unwrap(), bytes);
        }
    }

    #[test]
    fn checkpoint_symlinks_and_shared_inodes_are_not_absence_or_private_storage() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("checkpoints");
        let mut session = Session::open(&directory).unwrap().unwrap();
        let outside = root.path().join("outside");
        std::fs::write(&outside, b"outside").unwrap();
        symlink(&outside, directory.join(JOURNAL)).unwrap();
        assert!(session.read().is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"outside");
        drop(session);
        std::fs::remove_file(directory.join(JOURNAL)).unwrap();
        std::fs::hard_link(&outside, directory.join(JOURNAL)).unwrap();
        let mut session = Session::open(&directory).unwrap().unwrap();
        assert!(session.read().is_err());
        drop(session);
        let alias = root.path().join("alias");
        symlink(&directory, &alias).unwrap();
        assert!(Session::open(&alias).is_err());
    }

    #[test]
    fn checkpoint_initialization_refuses_missing_parent_before_creating_any_state() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("unavailable-storage-parent");
        assert!(Session::open(&parent.join("checkpoints")).is_err());
        assert!(!parent.exists());
    }

    #[test]
    fn checkpoint_wire_rejects_unknown_fields_oversized_and_truncated_frames() {
        for value in [
            json!({"version":1,"id":1,"command":"read","home":"forbidden"}),
            json!({"version":1,"id":1,"command":"write","ledger":[],"key":[]}),
        ] {
            let bytes = serde_json::to_vec(&value).unwrap();
            let mut input = [&(bytes.len() as u32).to_be_bytes()[..], &bytes].concat();
            assert!(receive(&mut input.as_slice()).is_err());
            input.pop();
            assert!(receive(&mut input.as_slice()).is_err());
        }
        assert!(receive(&mut ((MAX_BYTES + 1) as u32).to_be_bytes().as_slice()).is_err());
        assert!(receive(&mut [0_u8, 0, 0].as_slice()).is_err());
    }

    #[test]
    fn checkpoint_model_ledgers_round_trip_through_actual_durable_owner() {
        let proofs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../gents/proofs");
        let prepared = std::process::Command::new("lake")
            .args(["build", "Proofs.Conformance.EditorUndoCheckpoint"])
            .current_dir(&proofs)
            .output()
            .unwrap();
        assert!(
            prepared.status.success(),
            "{}{}",
            String::from_utf8_lossy(&prepared.stdout),
            String::from_utf8_lossy(&prepared.stderr)
        );
        let output = std::process::Command::new("lake")
            .args([
                "env",
                "lean",
                "--run",
                "Proofs/Conformance/EditorUndoCheckpoint.lean",
            ])
            .current_dir(proofs)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let cases: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let mut count = 0;
        for case in cases.as_array().unwrap() {
            for entry in case["trace"].as_array().unwrap() {
                let stored = &entry["expected"]["stored"];
                if stored["kind"] != "present" || stored["version"] != VERSION {
                    continue;
                }
                let expected: Vec<Operation> =
                    serde_json::from_value(stored["ledger"].clone()).unwrap();
                let root = tempfile::tempdir().unwrap();
                let directory = root.path().join("checkpoints");
                let mut session = Session::open(&directory).unwrap().unwrap();
                session.read().unwrap();
                session
                    .write(serde_json::from_value(stored["ledger"].clone()).unwrap())
                    .unwrap();
                drop(session);
                let mut reopened = Session::open(&directory).unwrap().unwrap();
                assert_eq!(reopened.read().unwrap().unwrap().ledger, expected);
                count += 1;
            }
        }
        assert!(count > 0);
    }
}
