use gents_observe::client_inference::{read_inference_setup_state_from, InferenceSetupState};
impl crate::client_runtime::ClientRuntime {
    pub async fn read_inference_setup_state(
        &self,
        node_did: &str,
    ) -> anyhow::Result<InferenceSetupState> {
        use anyhow::Context;
        let endpoint = self
            .operator_observation_graphql(node_did)
            .context("runtime connection is missing or ambiguous")?;
        read_inference_setup_state_from(&endpoint, node_did).await
    }
}
