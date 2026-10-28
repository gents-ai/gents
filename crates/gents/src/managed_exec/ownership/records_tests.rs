use std::io::Write;
use std::sync::{Arc, Mutex};

use super::{HostRecordStore, ProcessIdentity, ProcessRecord};

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Self;
    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

fn capture<T>(operation: impl FnOnce() -> T) -> (T, String) {
    crate::test_support::enable_scoped_event_capture();
    let output = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_max_level(tracing::Level::WARN)
        .with_writer(output.clone())
        .finish();
    let result = tracing::subscriber::with_default(subscriber, operation);
    let text = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
    (result, text)
}

fn record(key: &str) -> ProcessRecord {
    ProcessRecord {
        tool_call_id: format!("call-{key}"),
        tool_call_doc_id: key.to_owned(),
        identity: ProcessIdentity {
            pid: -1,
            pgid: -1,
            start: "fixture".to_owned(),
        },
    }
}

fn retained(directory: &std::path::Path, record: &ProcessRecord) {
    std::fs::write(
        super::record_path(directory, &record.tool_call_doc_id),
        serde_json::to_vec(record).unwrap(),
    )
    .unwrap();
}

#[test]
fn missing_directory_is_empty_without_warning_for_both_read_modes() {
    let directory = tempfile::tempdir().unwrap();
    let store = HostRecordStore::<ProcessRecord>::Durable(directory.path().join("missing"));
    let ((), output) = capture(|| {
        assert!(store.list().is_empty());
        assert!(store.try_list().unwrap().is_empty());
    });
    assert!(output.is_empty(), "{output}");
}

#[test]
fn unreadable_directory_warns_in_recovery_and_refuses_strict_verification() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("not-a-directory");
    std::fs::write(&path, b"blocked").unwrap();
    let store = HostRecordStore::<ProcessRecord>::Durable(path.clone());
    let ((), output) = capture(|| {
        assert!(store.list().is_empty());
        assert!(store.try_list().is_err());
    });
    assert!(
        output.contains("WARN"),
        "directory failure was silent: {output}"
    );
    assert!(
        output.contains(path.to_str().unwrap()),
        "directory is missing from the diagnostic: {output}"
    );
}

#[test]
fn recovery_keeps_valid_records_around_bad_entries_while_strict_reads_refuse() {
    let directory = tempfile::tempdir().unwrap();
    let store = HostRecordStore::<ProcessRecord>::Durable(directory.path().to_owned());
    let expected = [record("first"), record("last")];
    for row in &expected {
        retained(directory.path(), row);
    }
    std::fs::write(directory.path().join("bad.json"), b"invalid").unwrap();
    std::fs::create_dir(directory.path().join("unreadable.json")).unwrap();
    let mut actual = store.list();
    actual.sort_by(|a, b| a.tool_call_doc_id.cmp(&b.tool_call_doc_id));
    assert_eq!(actual, expected);
    assert!(store.try_list().is_err());
}

#[test]
fn strict_and_recovery_reads_ignore_staged_files_and_unrelated_entries() {
    let directory = tempfile::tempdir().unwrap();
    let store = HostRecordStore::<ProcessRecord>::Durable(directory.path().to_owned());
    let row = record("document/with spaces");
    retained(directory.path(), &row);
    std::fs::write(directory.path().join("staged.json.tmp"), b"partial").unwrap();
    std::fs::write(directory.path().join("other.txt"), b"other").unwrap();
    std::fs::create_dir(directory.path().join("unrelated")).unwrap();
    assert_eq!(store.list(), [row.clone()]);
    assert_eq!(store.try_list().unwrap(), [row]);
}

#[test]
fn volatile_store_clones_share_the_same_strict_and_recovery_view() {
    let store = HostRecordStore::default();
    let row = record("document");
    let clone = store.clone();
    store.write(&row).unwrap();
    assert_eq!(clone.list(), [row.clone()]);
    assert_eq!(clone.try_list().unwrap(), [row]);
    clone.remove("document");
    assert!(store.list().is_empty());
    assert!(store.try_list().unwrap().is_empty());
}
