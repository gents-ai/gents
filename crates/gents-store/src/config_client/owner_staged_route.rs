//! The runtime's signed owner-staged route (Lean
//! `Identity.OwnerStaged.overHttp`): a client that reaches the runtime over
//! HTTP asks it to decide one owner-staged change, and the runtime decides it
//! in an embedded transaction as that client. Every node a client writes
//! through over loopback HTTP mounts it, the serving runtime as well as the
//! in-process nodes of offline commands, so a client change is decided the
//! same way whichever node serves it.

use std::sync::{Arc, OnceLock, Weak};

use chrono::{SecondsFormat, Utc};
use defra_node::Node as EmbeddedNode;
use gents_protocol::owner_staged::{
    OwnerStagedChange, OwnerStagedCommand, OwnerStagedRefusal, OWNER_STAGED_COMMAND_VERSION,
    OWNER_STAGED_PATH,
};

use super::txn::StagedRelation;
use super::{ConfigAccess, ConfigClientError, GraphqlEndpoint, Result};
use crate::graphql::escape_graphql_string;

mod handler;

/// Send `change` as one signed command to the runtime `endpoint` serves,
/// signed with the endpoint's held client key. A refusal is the same
/// `ConfigClientError` the embedded decision returns.
pub(super) async fn request(
    endpoint: &GraphqlEndpoint,
    change: &OwnerStagedChange,
    grant: bool,
) -> Result<()> {
    let staged = StagedRelation::of(change)?;
    let capability = endpoint.held_client_signing_capability()?.clone();
    let mut command = OwnerStagedCommand {
        version: OWNER_STAGED_COMMAND_VERSION,
        audience: endpoint.audience()?,
        change: change.clone(),
        grant,
        signer_did: capability.did().to_owned(),
        issued_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        nonce: uuid::Uuid::new_v4().simple().to_string(),
        sig: Vec::new(),
    };
    command.sig = capability.sign(command.signing_payload()).await?;
    let mut url = reqwest::Url::parse(endpoint.url().trim()).map_err(|source| {
        ConfigClientError::ParseEndpoint {
            url: endpoint.url().to_owned(),
            source,
        }
    })?;
    url.set_path(OWNER_STAGED_PATH);
    url.set_query(None);
    url.set_fragment(None);
    let client = super::graphql::http_client().await?;
    let response = endpoint
        .authorize(client.post(url).json(&command))?
        .send()
        .await?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let body = response.text().await?;
    let code = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|body| serde_json::from_value::<OwnerStagedRefusal>(body["code"].clone()).ok());
    let (collection, doc_id) = (staged.collection.to_owned(), staged.doc_id);
    Err(match code {
        Some(OwnerStagedRefusal::Denied) => {
            ConfigClientError::OwnerStagedRelationshipDenied { collection, doc_id }
        }
        Some(OwnerStagedRefusal::Unmanaged) => {
            ConfigClientError::OwnerStagedRelationshipUnmanaged { collection, doc_id }
        }
        Some(OwnerStagedRefusal::OutOfKind) => {
            ConfigClientError::OwnerStagedTargetRefused { collection, doc_id }
        }
        None => ConfigClientError::OwnerStagedRouteStatus { status, body },
    })
}

/// The owner-staged route of one node. The node owns the HTTP server that
/// holds this route, so the route holds it weakly and answers 503 until
/// [`Self::serve`] binds it after the node starts.
#[derive(Clone, Default)]
pub struct OwnerStagedRoute {
    served: Arc<
        OnceLock<(
            Weak<EmbeddedNode>,
            crate::identity::RuntimeAgentProof,
        )>,
    >,
}

impl OwnerStagedRoute {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind the started node this route decides on and the runtime key that
    /// owns it; the route hands each verified client's decision only that
    /// key's derivation proof (`Identity.OwnerStaged.derivedAgents`). Only the
    /// first binding counts, and a key that is not the node's own identity
    /// leaves the route answering `starting`.
    pub fn serve(
        &self,
        node: &Arc<EmbeddedNode>,
        runtime: Arc<dyn crate::identity::NodeIdentity>,
    ) {
        if node.node_identity_did() != Some(runtime.did()) {
            tracing::error!(
                runtime_did = runtime.did(),
                node_did = node.node_identity_did(),
                "owner-staged route was handed a key that is not the node's identity"
            );
            return;
        }
        let _ = self.served.set((
            Arc::downgrade(node),
            crate::identity::RuntimeAgentProof::new(runtime),
        ));
    }

    /// The route, for `HttpConfig::with_extra_routes`.
    pub fn router(&self) -> axum::Router {
        axum::Router::new().route(
            OWNER_STAGED_PATH,
            axum::routing::post(handler::owner_staged_handler).with_state(self.clone()),
        )
    }

    fn node(&self) -> Option<(Arc<EmbeddedNode>, crate::identity::RuntimeAgentProof)> {
        let (node, proof) = self.served.get()?;
        Some((node.upgrade()?, proof.clone()))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SignedCommandNonceError {
    #[error("signed command nonce requires the configured runtime identity")]
    RuntimeIdentity,
    #[error(transparent)]
    RuntimeDid(#[from] identity::Error),
    #[error("signed command nonce was already consumed")]
    Consumed,
    #[error("persisting the signed command nonce replay fence")]
    Persist(#[source] Box<dyn std::error::Error + Send + Sync>),
}

const NONCE_RETENTION_SECONDS: i64 = 5 * 60;

/// Consume `signer_did`'s `nonce` once, as the runtime. Enrollment operator
/// commands and owner-staged commands share this one replay fence; a nonce
/// is retained for longer than any command it fences stays current.
pub async fn consume_signed_command_nonce(
    node: &Arc<EmbeddedNode>,
    signer_did: &str,
    nonce: &str,
    now: chrono::DateTime<Utc>,
) -> std::result::Result<(), SignedCommandNonceError> {
    let runtime = identity::Did::new(
        node.node_identity_did()
            .ok_or(SignedCommandNonceError::RuntimeIdentity)?,
    )?;
    let access = ConfigAccess::local_as(node.clone(), runtime.clone());
    let nonce_key = gents_protocol::enrollment::derive_enrollment_id(
        "gents-enrollment-operator-nonce-v1",
        &[signer_did, nonce],
    );
    let expires_at = (now + chrono::Duration::seconds(NONCE_RETENTION_SECONDS))
        .to_rfc3339_opts(SecondsFormat::Secs, true);
    let now = now.to_rfc3339_opts(SecondsFormat::Secs, true);
    let nonce_key_escaped = escape_graphql_string(&nonce_key);
    let signer_did = escape_graphql_string(signer_did);
    let now_escaped = escape_graphql_string(&now);
    let expires_at = escape_graphql_string(&expires_at);
    let mutation = format!(
        r#"mutation {{
            delete_EnrollmentOperatorNonce(filter: {{ expires_at: {{ _lte: "{now_escaped}" }} }}) {{ _docID }}
            create_EnrollmentOperatorNonce(input: {{
                nonce_key: "{nonce_key_escaped}", admin_did: "{signer_did}",
                expires_at: "{expires_at}", consumed_at: "{now_escaped}", parent_linked: false
            }}) {{ _docID }}
        }}"#
    );
    let written = access
        .transact_write("config.consume_signed_command_nonce", |txn| {
            let (mutation, runtime) = (&mutation, &runtime);
            Box::pin(async move {
                let response = txn.execute(mutation).await?;
                let doc =
                    crate::graphql::created_doc_id(&response, "EnrollmentOperatorNonce")?;
                super::maybe_link_deferred_operation_in_txn(
                    txn,
                    "EnrollmentOperatorNonce",
                    &doc,
                    runtime,
                )
                .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await;
    let Err(write_error) = written else {
        return Ok(());
    };
    let query = format!(
        r#"{{ EnrollmentOperatorNonce(filter: {{ nonce_key: {{ _eq: "{nonce_key_escaped}" }} }}) {{ nonce_key }} }}"#
    );
    let consumed = access.execute(&query).await.is_ok_and(|response| {
        gents_protocol::graphql::graphql_rows_from_response(&response, "EnrollmentOperatorNonce")
            .iter()
            .any(|row| row["nonce_key"].as_str() == Some(nonce_key.as_str()))
    });
    Err(if consumed {
        SignedCommandNonceError::Consumed
    } else {
        SignedCommandNonceError::Persist(write_error.into())
    })
}

#[cfg(test)]
mod tests;
