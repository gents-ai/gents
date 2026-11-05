use super::*;
use gents_protocol::file_changes::{FileChangeImages, FileImage, FilePermissions};
use gents_protocol::file_image_hydration::FileImageScope;
use gents_protocol::file_inventory::{
    FileInventoryAdmission, FileInventoryBinding, FileInventoryCoverage, FileInventorySnapshotView,
    FileInventoryViewCursor,
};
use gents_protocol::session_hydration::HydrationRequest;

fn fixture() -> (
    FileInventoryObservation,
    FileInventoryImageReference,
    ToolFileChanges,
) {
    let change = FileChangeImages {
        root: "/fixture".into(),
        path: "src/main.rs".into(),
        before: FileImage::Missing,
        after: FileImage::File {
            content_hash: "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                .into(),
            data: String::new(),
            permissions: FilePermissions {
                readonly: false,
                unix_mode: Some(0o644),
            },
            links: Some(1),
        },
    };
    let entry = FileInventoryEntry {
        request_coverage: None,
        order: None,
        tool_call_doc_id: "tool-physical".into(),
        request_doc_id: "request-physical".into(),
        scope: FileImageScope {
            requester_did: "origin-requester".into(),
            node_did: "origin-node".into(),
            session_id: "origin-session".into(),
        },
        projection_doc_id: "metadata-physical".into(),
        observation_doc_id: "image-physical".into(),
        change: FileChangeMetadata::from(&change),
    };
    let admission = FileInventoryAdmission {
        request: HydrationRequest {
            request_key: "target-request".into(),
            peer_id: "client-peer".into(),
            requester_did: "target-requester".into(),
            node_did: "target-node".into(),
            session_id: "target-session".into(),
        },
        session_doc_id: "target-physical".into(),
    };
    let reference = FileInventoryImageReference {
        cursor: FileInventoryViewCursor {
            attempt_id: "attempt-1".into(),
            snapshot_digest: "signed-snapshot".into(),
            offset: 0,
        },
        entry_index: 0,
    };
    let observation = FileInventoryObservation {
        admission: admission.clone(),
        attempt_id: "attempt-1".into(),
        current: Some(FileInventorySnapshotView {
            attempt_id: "attempt-1".into(),
            binding: FileInventoryBinding {
                admission,
                digest: "signed-snapshot".into(),
                coverage: FileInventoryCoverage {
                    entries: 1,
                    pending: 0,
                    unknown: 0,
                },
            },
            received_entries: 1,
            complete: true,
            known_inventory: true,
            offset: 0,
            entries: vec![entry],
            next: None,
        }),
        previous: None,
        refusal: None,
    };
    (
        observation,
        reference,
        ToolFileChanges {
            version: FILE_CHANGES_VERSION,
            tool_call_doc_id: "tool-physical".into(),
            changes: vec![change],
        },
    )
}

#[test]
fn selected_record_keeps_its_origin_and_exact_received_coordinate() {
    let (mut observation, reference, _) = fixture();
    let expected = observation.current.as_ref().unwrap().entries[0].clone();
    assert_eq!(selected_entry(&observation, &reference).unwrap(), &expected);
    observation.attempt_id = "refresh-attempt".into();
    observation.previous = observation.current.take();
    assert_eq!(selected_entry(&observation, &reference).unwrap(), &expected);
    for changed in [
        FileInventoryImageReference {
            entry_index: 1,
            ..reference.clone()
        },
        FileInventoryImageReference {
            cursor: FileInventoryViewCursor {
                attempt_id: "other".into(),
                ..reference.cursor.clone()
            },
            ..reference.clone()
        },
        FileInventoryImageReference {
            cursor: FileInventoryViewCursor {
                snapshot_digest: "other".into(),
                ..reference.cursor.clone()
            },
            ..reference.clone()
        },
        FileInventoryImageReference {
            cursor: FileInventoryViewCursor {
                offset: 64,
                ..reference.cursor.clone()
            },
            ..reference.clone()
        },
    ] {
        assert!(selected_entry(&observation, &changed).is_err());
    }
}

#[test]
fn image_binding_rejects_changed_observation_tool_path_and_hash() {
    let (observation, reference, changes) = fixture();
    let entry = selected_entry(&observation, &reference).unwrap();
    assert!(match_observation(&reference, entry.clone(), "other-image", changes.clone()).is_err());
    let mut wrong = changes.clone();
    wrong.tool_call_doc_id = "other-tool".into();
    assert!(match_observation(&reference, entry.clone(), "image-physical", wrong).is_err());
    let mut wrong = changes.clone();
    wrong.version += 1;
    assert!(match_observation(&reference, entry.clone(), "image-physical", wrong).is_err());
    let mut wrong = changes.clone();
    wrong.changes[0].path = "other-file".into();
    assert!(match_observation(&reference, entry.clone(), "image-physical", wrong).is_err());
    let mut wrong = changes.clone();
    wrong.changes[0].after = FileImage::Missing;
    assert!(match_observation(&reference, entry.clone(), "image-physical", wrong).is_err());
    let mut wrong = changes.clone();
    wrong.changes.push(wrong.changes[0].clone());
    assert!(match_observation(&reference, entry.clone(), "image-physical", wrong).is_err());
}

#[test]
fn a_single_recorded_file_is_returned_from_a_capture_larger_than_one_metadata_page() {
    let (observation, reference, mut changes) = fixture();
    let entry = selected_entry(&observation, &reference).unwrap();
    let expected = changes.changes[0].clone();
    for index in 0..64 {
        let mut other = expected.clone();
        other.path = format!("other-{index}.rs");
        changes.changes.push(other);
    }
    let image = match_observation(&reference, entry.clone(), "image-physical", changes).unwrap();
    assert_eq!(image.reference, reference);
    assert_eq!(image.entry, *entry);
    assert_eq!(image.change, expected);
}
