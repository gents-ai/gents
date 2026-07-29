//! Phase D: rolling-upgrade policy helpers.

use gents_migration::{is_unknown_version_read_error, ROLLING_UPGRADE_GUIDANCE};

#[test]
fn guidance_mentions_promote_nodes() {
    assert!(ROLLING_UPGRADE_GUIDANCE.contains("promote"));
    assert!(ROLLING_UPGRADE_GUIDANCE.contains("UnknownLineage"));
}

#[test]
fn classifies_defradb_unknown_version_messages() {
    assert!(is_unknown_version_read_error(
        "QueryError: no migration path found for document bae-1 from version bafkreiA to bafkreiB"
    ));
    assert!(!is_unknown_version_read_error("collection not found"));
}
