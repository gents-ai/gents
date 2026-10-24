use super::*;

impl RequestLifecycle {
    pub(super) fn ensure_state(
        &self,
        expected: &[LocalLifecycleState],
        action: &str,
    ) -> Result<(), LifecycleError> {
        if expected.contains(&self.state) {
            return Ok(());
        }

        Err(LifecycleError::LifecycleStateRejected {
            action: action.to_owned(),
            request_id: self.request.request_id.clone(),
            state: format!("{:?}", self.state),
        })
    }
}
