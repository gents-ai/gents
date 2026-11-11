use std::io::Write;
use std::sync::{Arc, Mutex};

use super::{HostRecordStore, ProcessIdentity, ProcessRecord};

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
    assert_eq!(store.list(), std::slice::from_ref(&row));
    assert_eq!(store.try_list().unwrap(), [row]);
}

