use super::{graphql, graphql_api_base, ConfigAccess, ConfigClientError};

impl ConfigAccess {
    /// Registers `policy`, the text of a DefraDB document access policy, with
    /// this access's actor as its creator, and returns its ID.
    ///
    /// External premise (pinned DefraDB `add_dac_policy_as`): the actor needs
    /// the `add-dac-policy` node permission, and a local ACP names each
    /// registration by the next policy counter, so the same text registered
    /// twice is two policies.
    pub async fn add_document_policy(&self, policy: &str) -> Result<String, ConfigClientError> {
        match self {
            Self::Local(local) => {
                local.require_actor_signer()?;
                let context = local.request_context();
                let policy = policy.to_owned();
                local
                    .run_owned(move |node| async move {
                        Ok::<_, ConfigClientError>(node.add_dac_policy_as(&context, &policy).await?)
                    })
                    .await
            }
            Self::Graphql(endpoint) => {
                #[derive(serde::Deserialize)]
                struct Added {
                    #[serde(rename = "PolicyID")]
                    policy_id: String,
                }
                let url = format!("{}/acp/document/policy", graphql_api_base(endpoint.url())?);
                let client = graphql::http_client().await?;
                let response = endpoint
                    .send_resolving(|endpoint| {
                        endpoint.authorize(client.post(&url).body(policy.to_owned()))
                    })
                    .await??;
                let status = response.status();
                if !status.is_success() {
                    return Err(ConfigClientError::HttpOperationStatus {
                        operation: "add document policy",
                        status,
                    });
                }
                Ok(response.json::<Added>().await?.policy_id)
            }
        }
    }
}
