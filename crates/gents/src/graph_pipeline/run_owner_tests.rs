use super::*;

pub(super) async fn capture_failure_txn(
    txn: ConfigApplyTxn<'_>,
    view: &GraphRunView,
) -> Result<()> {
    match capture_failure_in_txn(&txn, view).await {
        Ok(false) => Ok(txn.discard().await?),
        Ok(true) => match txn.commit().await {
            Ok(()) => Ok(()),
            Err(error) if crate::config_client::is_classified_transaction_conflict(&error) => {
                Ok(())
            }
            Err(error) => Err(error.into()),
        },
        Err(error) => {
            let _ = txn.discard().await;
            if crate::config_client::is_classified_transaction_conflict(&error) {
                Ok(())
            } else {
                Err(error)
            }
        }
    }
}

pub(super) async fn commit_terminal_txn(
    txn: ConfigApplyTxn<'_>,
    view: &GraphRunView,
    status: &str,
) -> Result<()> {
    let completed_at = chrono::Utc::now().to_rfc3339();
    match commit_terminal_in_txn(&txn, view, status, &completed_at).await {
        Ok(()) => Ok(txn.commit().await?),
        Err(error) => {
            let _ = txn.discard().await;
            Err(error)
        }
    }
}

pub(super) async fn persist_cancellation_intent(
    txn: ConfigApplyTxn<'_>,
    actor_did: &str,
    run_id: &str,
    reason: Option<&str>,
) -> Result<()> {
    let requested_at = chrono::Utc::now().to_rfc3339();
    match persist_cancellation_intent_in_txn(&txn, actor_did, run_id, reason, &requested_at).await {
        Ok(()) => Ok(txn.commit().await?),
        Err(error) => {
            let _ = txn.discard().await;
            Err(error)
        }
    }
}
