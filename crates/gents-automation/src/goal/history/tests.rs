use super::*;
use crate::test_support::doubles::{HistoryReader, UnchangedAssignmentReader};
use std::sync::Mutex;

#[tokio::test]
async fn retained_history_is_complete_across_bounded_reconstruction_queries() {
    let reader = HistoryReader {
        calls: Mutex::new(Vec::new()),
        fault: None,
    };
    let versions = load_goal_versions_for_session(&reader, "owner", "chat")
        .await
        .unwrap();
    assert_eq!(versions.len(), 67);
    for (index, version) in versions.iter().enumerate() {
        assert_eq!(version.commit_cid, format!("commit-{index:03}"));
        assert_eq!(version.goal.objective, version.commit_cid);
        assert_eq!(version.height, index as i64 + 1);
        assert_eq!(
            version.parent_commit_cids,
            if index == 0 {
                vec![]
            } else {
                vec![format!("commit-{:03}", index - 1)]
            }
        );
    }
    let calls = reader.calls.lock().unwrap();
    assert!(calls.len() > 1);
    assert!(calls.iter().all(|batch| batch.len() <= 32));
    assert_eq!(calls.iter().flatten().count(), versions.len());
    let projected = versions
        .into_iter()
        .map(|version| {
            GoalAssignmentVersion::from_snapshot(
                version.goal,
                version.commit_cid,
                version.height,
                version.parent_commit_cids,
            )
        })
        .collect::<Vec<_>>();
    let latest = super::super::latest_assignment_goals(&projected).unwrap();
    assert_eq!(latest.len(), 1);
    assert_eq!(latest[0].objective, "commit-066");
}

#[tokio::test]
async fn unavailable_or_incomplete_batches_never_return_partial_assignment_history() {
    for (fault, expected) in [
        ("unavailable", "batch 2"),
        ("omitted", "snapshots for 32 visible commits"),
        ("foreign", "belongs to a different owner"),
    ] {
        let reader = HistoryReader {
            calls: Mutex::new(Vec::new()),
            fault: Some(fault),
        };
        let error = load_goal_versions_for_session(&reader, "owner", "chat")
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains(expected), "{error:#}");
        if fault == "unavailable" {
            assert!(format!("{error:#}").contains("retained block unavailable"));
        }
    }
}

#[tokio::test]
async fn unknown_or_changed_projection_metadata_requires_native_reconstruction() {
    for fault in [
        "missing_link",
        "unknown_link",
        "immutable",
        "assignment",
        "merge",
    ] {
        let reader = UnchangedAssignmentReader {
            metadata_fault: Some(fault),
            inner: HistoryReader {
                calls: Mutex::new(Vec::new()),
                fault: None,
            },
        };
        let versions = load_assignment_versions_for_session(&reader, "owner", "chat")
            .await
            .unwrap();
        assert_eq!(versions.len(), 67);
        let calls = reader.inner.calls.lock().unwrap();
        assert_eq!(
            calls
                .iter()
                .flatten()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["commit-000", "commit-020", "commit-066"],
            "{fault}"
        );
    }
}

#[tokio::test]
async fn projection_failures_never_supply_partial_assignment_evidence() {
    for (fault, expected) in [
        ("unavailable", "retained block unavailable"),
        ("omitted", "snapshots for 1 visible commits"),
        ("foreign", "belongs to a different owner"),
        ("projection", "disagrees with its native frontier"),
        ("missing_parent", "history is incomplete"),
        ("parent_height", "parent metadata disagrees"),
    ] {
        let reader = UnchangedAssignmentReader {
            metadata_fault: Some(fault),
            inner: HistoryReader {
                calls: Mutex::new(Vec::new()),
                fault: Some(fault),
            },
        };
        let error = load_assignment_versions_for_session(&reader, "owner", "chat")
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains(expected),
            "{fault}: {error:#}"
        );
    }
}
