use super::*;

#[test]
fn plugin_writes_failure_keeps_a_typed_conflict_replayable() {
    let conflict = crate::config_client::classified_conflict_for_test();
    let wrapped = plugin_writes_failed(conflict.into());
    assert!(crate::config_client::is_classified_transaction_conflict(
        &wrapped
    ));
    assert!(wrapped.downcast_ref::<PluginWritesFailed>().is_none());

    let refused = plugin_writes_failed(anyhow::anyhow!("collection is not declared"));
    assert!(!crate::config_client::is_classified_transaction_conflict(
        &refused
    ));
    assert!(refused.downcast_ref::<PluginWritesFailed>().is_some());
}
