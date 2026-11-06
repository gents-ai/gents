use std::sync::Arc;

use defra_node::Node as EmbeddedNode;
use serde::{Deserialize, Serialize};

/// This value is stored only in the native `secret_configuration` resource.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum BackendCredentialSecret {
    ApiKey {
        key: String,
    },
    AwsCredentials {
        access_key_id: String,
        secret_access_key: String,
        session_token: Option<String>,
    },
    GoogleCredentials {
        credentials_json: String,
    },
}

impl std::fmt::Debug for BackendCredentialSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BackendCredentialSecret([redacted])")
    }
}

impl BackendCredentialSecret {
    pub(crate) fn into_backend_auth(self) -> crate::documents::BackendAuth {
        use crate::documents::BackendAuth;
        match self {
            Self::ApiKey { key } => BackendAuth::ApiKey { key },
            Self::AwsCredentials {
                access_key_id,
                secret_access_key,
                session_token,
            } => BackendAuth::AwsCredentials {
                access_key_id,
                secret_access_key,
                session_token,
            },
            Self::GoogleCredentials { credentials_json } => {
                BackendAuth::GoogleCredentials { credentials_json }
            }
        }
    }

    pub(crate) fn from_backend_auth(auth: crate::documents::BackendAuth) -> Option<Self> {
        use crate::documents::BackendAuth;
        match auth {
            BackendAuth::ApiKey { key } => Some(Self::ApiKey { key }),
            BackendAuth::AwsCredentials {
                access_key_id,
                secret_access_key,
                session_token,
            } => Some(Self::AwsCredentials {
                access_key_id,
                secret_access_key,
                session_token,
            }),
            BackendAuth::GoogleCredentials { credentials_json } => {
                Some(Self::GoogleCredentials { credentials_json })
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct BackendCredential {
    pub credential_id: String,
    pub node_did: String,
    pub provider_kind: String,
    pub endpoint: String,
    /// Fresh opaque value supplied on each committed rotation, never a secret hash.
    pub version: String,
    pub secret: BackendCredentialSecret,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum BackendCredentialError {
    #[error("backend credential is absent or ambiguous")]
    MissingOrAmbiguous,
    #[error(
        "backend credential does not match the approved Node, provider, destination, or version"
    )]
    BindingMismatch,
    #[error("backend credential could not be read")]
    Read(#[source] anyhow::Error),
    #[error("backend credential has an invalid stored shape")]
    Invalid,
    #[error("backend credential rotation must use a fresh nonempty version")]
    InvalidRotation,
}

impl BackendCredential {
    /// Mint before signing the transaction's attempt; signed dispatch prevents
    /// blind replay, and the public value is never derived from the secret.
    pub(crate) fn for_rotation(
        credential_id: Option<String>,
        node_did: String,
        provider_kind: String,
        endpoint: String,
        secret: BackendCredentialSecret,
    ) -> Self {
        Self {
            credential_id: credential_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            node_did,
            provider_kind,
            endpoint,
            version: uuid::Uuid::new_v4().to_string(),
            secret,
        }
    }

    pub(crate) fn matches_binding(
        &self,
        node: &str,
        provider: &str,
        endpoint: &str,
        version: &str,
    ) -> bool {
        !version.is_empty()
            && self.node_did == node
            && self.provider_kind == provider
            && self.endpoint == endpoint
            && self.version == version
    }
}

pub(crate) async fn backend_credential_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    credential_id: &str,
) -> Result<BackendCredential, BackendCredentialError> {
    let id = crate::store::graphql::escape_graphql_string(credential_id);
    let query = format!(
        r#"{{ BackendCredential(filter: {{credential_id: {{_eq: "{id}"}}}}, limit: 2) {{ credential_id node_did provider_kind endpoint version secret }} }}"#
    );
    let response = txn
        .execute(&query)
        .await
        .map_err(|error| BackendCredentialError::Read(error.into()))?;
    let rows = gents_protocol::graphql::graphql_rows_from_response(&response, "BackendCredential");
    if rows.len() != 1 {
        return Err(BackendCredentialError::MissingOrAmbiguous);
    }
    let credential: BackendCredential =
        serde_json::from_value(rows[0].clone()).map_err(|_| BackendCredentialError::Invalid)?;
    if credential.credential_id != credential_id {
        return Err(BackendCredentialError::BindingMismatch);
    }
    Ok(credential)
}

/// Use the same transaction as the approved metadata change. The actor and
/// native secret-configuration ACP remain those of the supplied transaction.
pub(crate) async fn put_backend_credential_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    credential: &BackendCredential,
) -> anyhow::Result<String> {
    if credential.version.is_empty()
        || credential.credential_id.is_empty()
        || credential.node_did.is_empty()
        || credential.provider_kind.is_empty()
        || credential.endpoint.is_empty()
    {
        return Err(BackendCredentialError::InvalidRotation.into());
    }
    let id = crate::store::graphql::escape_graphql_string(&credential.credential_id);
    let query = format!(
        r#"{{ BackendCredential(filter: {{credential_id: {{_eq: "{id}"}}}}, limit: 2) {{ _docID node_did version }} }}"#
    );
    let response = txn.execute(&query).await?;
    let rows = gents_protocol::graphql::graphql_rows_from_response(&response, "BackendCredential");
    if rows.len() > 1 {
        return Err(BackendCredentialError::MissingOrAmbiguous.into());
    }
    let mut input = serde_json::to_value(credential)?;
    if let Some(row) = rows.first() {
        if row["node_did"].as_str() != Some(credential.node_did.as_str()) {
            return Err(BackendCredentialError::BindingMismatch.into());
        }
        if row["version"].as_str() == Some(credential.version.as_str()) {
            return Err(BackendCredentialError::InvalidRotation.into());
        }
        let doc = row["_docID"]
            .as_str()
            .ok_or(BackendCredentialError::MissingOrAmbiguous)?;
        let fields = input
            .as_object_mut()
            .ok_or(BackendCredentialError::MissingOrAmbiguous)?;
        fields.remove("credential_id");
        fields.remove("node_did");
        let escaped = crate::store::graphql::escape_graphql_string(doc);
        let mutation = format!(
            r#"mutation($input: BackendCredentialMutationInputArg!) {{ update_BackendCredential(filter: {{_docID: {{_eq: "{escaped}"}}}}, input: $input) {{ _docID }} }}"#
        );
        let response = txn
            .execute_client_signed_update(
                &mutation,
                &serde_json::json!({"input": input}),
                gents_protocol::schemas::BACKEND_CREDENTIAL,
            )
            .await?;
        let changed =
            gents_protocol::graphql::extract_mutation_doc_id(&response, "BackendCredential")?;
        if changed != doc {
            return Err(BackendCredentialError::MissingOrAmbiguous.into());
        }
        Ok(changed)
    } else {
        let response = txn.execute_client_signed_create(
            "mutation($input: BackendCredentialMutationInputArg!) { create_BackendCredential(input: $input) { _docID } }",
            &serde_json::json!({"input": input}), gents_protocol::schemas::BACKEND_CREDENTIAL,
        ).await?;
        let doc = gents_protocol::graphql::extract_mutation_doc_id(&response, "BackendCredential")?;
        crate::store::config_client::publish_secret_configuration_in_txn(
            txn,
            "BackendCredential",
            &doc,
            &credential.node_did,
        )
        .await?;
        Ok(doc)
    }
}

/// The caller must first authorize the initiating actor's metadata selection.
/// This runtime-only lookup never substitutes another credential or destination.
pub(crate) async fn resolve_backend_credential(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    credential_id: &str,
    provider: &str,
    endpoint: &str,
    version: &str,
) -> Result<BackendCredential, BackendCredentialError> {
    let access = super::rows::runtime_credential_access(node, node_did)
        .map_err(BackendCredentialError::Read)?;
    let id = crate::store::graphql::escape_graphql_string(credential_id);
    let query = format!(
        r#"{{ BackendCredential(filter: {{credential_id: {{_eq: "{id}"}}}}, limit: 2) {{ credential_id node_did provider_kind endpoint version secret }} }}"#
    );
    let response = access
        .execute(&query)
        .await
        .map_err(|error| BackendCredentialError::Read(error.into()))?;
    let rows = gents_protocol::graphql::graphql_rows_from_response(&response, "BackendCredential");
    if rows.len() != 1 {
        return Err(BackendCredentialError::MissingOrAmbiguous);
    }
    let credential: BackendCredential =
        serde_json::from_value(rows[0].clone()).map_err(|_| BackendCredentialError::Invalid)?;
    if credential.credential_id != credential_id
        || !credential.matches_binding(node_did, provider, endpoint, version)
    {
        return Err(BackendCredentialError::BindingMismatch);
    }
    Ok(credential)
}

/// Resolve only after the caller has authorized the selected metadata.
pub(crate) async fn resolve_runtime_backend_auth(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    provider: crate::inference::backends::provider::BackendProviderKind,
    endpoint: &str,
    auth: &crate::documents::BackendAuth,
) -> anyhow::Result<crate::documents::BackendAuth> {
    let crate::documents::BackendAuth::Credential { id, version } = auth else {
        return Ok(auth.clone());
    };
    let secret =
        resolve_backend_credential(node, node_did, id, provider.as_str(), endpoint, version)
            .await?;
    let resolved = secret.secret.into_backend_auth();
    anyhow::ensure!(
        resolved.suits_provider(provider),
        "backend credential is incompatible with the provider"
    );
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_is_bound_to_destination_and_rotation() {
        let credential = BackendCredential {
            credential_id: "credential".into(),
            node_did: "node".into(),
            provider_kind: "provider".into(),
            endpoint: "https://provider.example".into(),
            version: "rotation-1".into(),
            secret: BackendCredentialSecret::ApiKey {
                key: "private".into(),
            },
        };
        assert!(credential.matches_binding(
            "node",
            "provider",
            "https://provider.example",
            "rotation-1"
        ));
        assert!(!credential.matches_binding(
            "other",
            "provider",
            "https://provider.example",
            "rotation-1"
        ));
        assert!(!credential.matches_binding(
            "node",
            "other",
            "https://provider.example",
            "rotation-1"
        ));
        assert!(!credential.matches_binding(
            "node",
            "provider",
            "https://other.example",
            "rotation-1"
        ));
        assert!(!credential.matches_binding(
            "node",
            "provider",
            "https://provider.example",
            "rotation-2"
        ));
        assert!(!format!("{credential:?}").contains("private"));
    }

    #[test]
    fn modeled_binding_cases_match_private_resolver() {
        let modeled = crate::lean_vocab_test::lean_identity_household_cases()
            .iter()
            .find(|case| case.name == "configuration-explicit-grants")
            .expect("configuration contract");
        let credential = BackendCredential {
            credential_id: "credential".into(),
            node_did: "node".into(),
            provider_kind: "provider".into(),
            endpoint: "https://provider.example".into(),
            version: "rotation-1".into(),
            secret: BackendCredentialSecret::ApiKey {
                key: "private".into(),
            },
        };
        for name in [
            "authorized",
            "foreign-node",
            "unapproved-destination",
            "rotated-credential",
        ] {
            let case = modeled
                ._backend_credential_cases
                .iter()
                .find(|case| case["name"] == name)
                .expect("modeled binding case");
            let node = if name == "foreign-node" {
                "other"
            } else {
                "node"
            };
            let endpoint = if name == "unapproved-destination" {
                "https://other.example"
            } else {
                "https://provider.example"
            };
            let version = if name == "rotated-credential" {
                "rotation-2"
            } else {
                "rotation-1"
            };
            assert_eq!(
                credential.matches_binding(node, "provider", endpoint, version),
                case["may_use"].as_bool().expect("modeled result"),
                "{name}"
            );
        }
    }
}
