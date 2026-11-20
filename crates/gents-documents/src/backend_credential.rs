use serde::{Deserialize, Serialize};

/// This value is stored only in the native `secret_configuration` resource.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BackendCredentialSecret {
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
    pub fn into_backend_auth(self) -> super::BackendAuth {
        use super::BackendAuth;
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

    pub(crate) fn from_backend_auth(auth: super::BackendAuth) -> Option<Self> {
        use super::BackendAuth;
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
pub struct BackendCredential {
    pub credential_id: String,
    pub node_did: String,
    pub provider_kind: String,
    pub endpoint: String,
    /// Fresh opaque value supplied on each committed rotation, never a secret hash.
    pub version: String,
    pub secret: BackendCredentialSecret,
}

#[derive(Debug, thiserror::Error)]
pub enum BackendCredentialError {
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

    pub fn matches_binding(
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

pub async fn backend_credential_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    credential_id: &str,
) -> Result<BackendCredential, BackendCredentialError> {
    let id = gents_store::graphql::escape_graphql_string(credential_id);
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
pub async fn put_backend_credential_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
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
    let id = gents_store::graphql::escape_graphql_string(&credential.credential_id);
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
        let escaped = gents_store::graphql::escape_graphql_string(doc);
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
        crate::publish_secret_configuration_in_txn(
            txn,
            "BackendCredential",
            &doc,
            &credential.node_did,
        )
        .await?;
        Ok(doc)
    }
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
        gents_lean_vocab::families::household_backend_credential_binding::replay(|case| {
            let node = if case.same_node { "node" } else { "other" };
            let endpoint = if case.destination_approved {
                "https://provider.example"
            } else {
                "https://other.example"
            };
            let version = if case.credential_version_matches {
                "rotation-1"
            } else {
                "rotation-2"
            };
            credential.matches_binding(node, "provider", endpoint, version)
        });
    }

    #[tokio::test]
    async fn backend_and_credential_documents_follow_modeled_native_acp() -> anyhow::Result<()> {
        use gents_store::config_client::ConfigAccess;
        use gents_store::graphql::{created_doc_id, escape_graphql_string};
        use gents_store::identity::NodeIdentity;
        let directory = tempfile::tempdir()?;
        let key = |name: &str| {
            gents_store::identity::KeyIdentity::load_or_create(directory.path().join(name), None)
        };
        let (user, agent, runtime) = (key("user")?, key("agent")?, key("runtime")?);
        let node = std::sync::Arc::new(
            defra_node::Node::builder()
                .with_node_identity_did(user.did())
                .build()
                .await?,
        );
        let context =
            defra_core::RequestContext::default().with_identity(Some(user.did().to_owned()));
        node.add_dac_policy_with_counter_as(
            &context,
            gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
            0,
        )
        .await?;
        let owner = ConfigAccess::local_as(node.clone(), identity::Did::new(user.did())?);
        for schema in [
            gents_protocol::schemas::NODE,
            gents_protocol::schemas::INFERENCE_BACKEND_METADATA,
            gents_protocol::schemas::BACKEND_CREDENTIAL,
        ] {
            owner.add_collection(schema).await?;
        }
        let node_did = escape_graphql_string(runtime.did());
        let node_doc = created_doc_id(
            &owner
                .write(
                    "test.credential.node",
                    &format!(
                        "mutation {{create_Node(input:{{node_did:\"{node_did}\"}}){{_docID}}}}"
                    ),
                )
                .await?,
            "Node",
        )?;
        let backend = created_doc_id(
            &owner
                .write(
                    "test.credential.backend",
                    &format!("mutation {{create_InferenceBackendMetadata(input:{{node_did:\"{node_did}\",backend_id:\"backend\",name:\"backend\",provider_kind:\"OpenAiCompatible\",endpoint:\"http://localhost:8000/v1\",auth_kind:\"unauthenticated\"}}){{_docID}}}}"),
                )
                .await?,
            "InferenceBackendMetadata",
        )?;
        let credential = created_doc_id(
            &owner
                .write(
                    "test.credential.secret",
                    &format!("mutation {{create_BackendCredential(input:{{node_did:\"{node_did}\",credential_id:\"credential\"}}){{_docID}}}}"),
                )
                .await?,
            "BackendCredential",
        )?;
        let agent_did = identity::Did::new(agent.did())?;
        owner
            .add_resource_relationship(
                "InferenceBackendMetadata",
                &backend,
                "parent",
                "nodes",
                &node_doc,
            )
            .await?;
        owner
            .add_actor_relationship("InferenceBackendMetadata", &backend, "reader", &agent_did)
            .await?;
        owner
            .add_resource_relationship(
                "BackendCredential",
                &credential,
                "parent",
                "nodes",
                &node_doc,
            )
            .await?;
        owner
            .add_actor_relationship(
                "BackendCredential",
                &credential,
                "runtime_writer",
                &identity::Did::new(runtime.did())?,
            )
            .await?;
        let mut revoked = false;
        for case in gents_lean_vocab::families::household_backend_credential_native::cases() {
            if case.input.revoked && !revoked {
                revoked = true;
                owner
                    .delete_actor_relationship(
                        "InferenceBackendMetadata",
                        &backend,
                        "reader",
                        &agent_did,
                    )
                    .await?;
            }
            let actor = match case.input.actor.as_str() {
                "user" => user.did(),
                "agent" => agent.did(),
                "runtime" => runtime.did(),
                actor => anyhow::bail!("unmodeled actor {actor}"),
            };
            let doc = match case.input.doc.as_str() {
                "backend" => &backend,
                "credential" => &credential,
                doc => anyhow::bail!("unmodeled document {doc}"),
            };
            let permission = match case.input.permission.as_str() {
                "read" => defra_node::DocumentPermission::Read,
                "update" => defra_node::DocumentPermission::Update,
                permission => anyhow::bail!("unmodeled permission {permission}"),
            };
            let allowed = node
                .document_acp()
                .check_doc_access(
                    &defra_node::AcpIdentity::Authenticated(actor.parse()?),
                    permission,
                    gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID,
                    &case.input.resource,
                    doc,
                )
                .await?;
            case.check(allowed);
        }
        node.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn backend_credential_and_its_selection_publish_together() -> anyhow::Result<()> {
        use gents_store::config_client::{ConfigAccess, ConfigApplyTxn};
        use gents_store::identity::NodeIdentity;
        for case in gents_lean_vocab::families::household_backend_credential_publication::cases() {
            let (builder, runtime) = gents_store::test_support::keyed_node_builder();
            let node = std::sync::Arc::new(builder.with_node_acp_enabled().build().await?);
            gents_store::schema::ensure_runtime_schemas(&node).await?;
            let owner = ConfigAccess::local_runtime(node.clone(), runtime.clone())?;
            let node_did = runtime.did().to_owned();
            crate::test_support::bootstrap_signed_test_node(&owner, &node_did).await;
            let endpoint = "http://127.0.0.1:1/v1";
            let publication = |metadata: &str, secret: &str| -> anyhow::Result<_> {
                let backend: crate::InferenceBackend = serde_json::from_value(serde_json::json!({
                    "node_did": node_did, "backend_id": "backend", "name": "backend",
                    "provider_kind": "OpenAiCompatible", "endpoint": endpoint,
                    "auth": {"kind": "credential", "id": "credential", "version": metadata},
                }))?;
                let metadata = crate::InferenceBackendMetadata::from_backend(
                    backend,
                    crate::BackendAuthSelection::Credential {
                        id: "credential".into(),
                        version: metadata.into(),
                    },
                )?;
                let credential = BackendCredential {
                    credential_id: "credential".into(),
                    node_did: node_did.clone(),
                    provider_kind: "OpenAiCompatible".into(),
                    endpoint: endpoint.into(),
                    version: secret.into(),
                    secret: BackendCredentialSecret::ApiKey {
                        key: "private".into(),
                    },
                };
                Ok((metadata, credential))
            };
            let publish = |access: ConfigAccess, metadata, credential| async move {
                access
                    .transact_write("test.credential_publication", |txn| {
                        let (metadata, credential) = (&metadata, &credential);
                        Box::pin(async move {
                            crate::inference_backend::write_backend_metadata_in_txn(
                                txn,
                                metadata,
                                Some(credential),
                            )
                            .await
                        })
                    })
                    .await
            };
            let (metadata, credential) = publication("old", "old")?;
            publish(owner.clone(), metadata, credential).await?;
            let (metadata, credential) = publication(&case.input.metadata, &case.input.secret)?;
            let actor = if case.input.authorized {
                owner.clone()
            } else {
                let stranger = tokio::task::spawn_blocking(|| {
                    gents_store::test_support::test_identity("credential-stranger")
                })
                .await?;
                ConfigAccess::local_as(node.clone(), identity::Did::new(stranger.did())?)
            };
            if case.input.committed {
                let _ = publish(actor, metadata, credential).await;
            } else {
                let (rolled_back, _) = ConfigApplyTxn::with_successful_mutation_failure_at(
                    Some(2),
                    publish(actor, metadata, credential),
                )
                .await;
                anyhow::ensure!(rolled_back.is_err(), "the failed commit must not publish");
            }
            let observed = owner
                .execute("{ InferenceBackendMetadata { credential_version } BackendCredential { version } }")
                .await?;
            let version = |collection: &str, field: &str| -> anyhow::Result<String> {
                let rows =
                    gents_protocol::graphql::graphql_rows_from_response(&observed, collection);
                anyhow::ensure!(rows.len() == 1, "one {collection} row: {observed}");
                rows[0][field]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| anyhow::anyhow!("{collection} has no {field}: {observed}"))
            };
            case.check(
                gents_lean_vocab::families::household_backend_credential_publication::CredentialVersions {
                    metadata: version("InferenceBackendMetadata", "credential_version")?,
                    secret: version("BackendCredential", "version")?,
                },
            );
            node.shutdown().await?;
        }
        Ok(())
    }
}
