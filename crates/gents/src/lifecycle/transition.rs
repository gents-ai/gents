use super::*;

impl RequestLifecycle {
    pub(super) fn ensure_state(
        &self,
        expected: &[LocalLifecycleState],
        action: &str,
    ) -> Result<()> {
        if expected.contains(&self.state) {
            return Ok(());
        }

        anyhow::bail!(
            "cannot {} request_id={} while lifecycle is in {:?}",
            action,
            self.request.request_id,
            self.state
        )
    }
}
