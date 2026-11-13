use std::time::Duration;

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use gents_store::config_client::document_decide::{
    decide_document_access, DocumentAccessQuestion, DocumentDecideError,
};
use serde::{Deserialize, Serialize};

use crate::http::router::RuntimeHttpState;

#[derive(Debug, Deserialize)]
pub struct IdentityDecideRequest {
    actor: String,
    permission: String,
    resource: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IdentityDecideResponse {
    allowed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

#[derive(Debug)]
struct ParsedResource {
    policy_id: String,
    resource_name: String,
    doc_id: String,
}

enum IdentityDecideError {
    BadRequest(String),
    Backend(String),
    Internal(String),
}

pub async fn identity_decide_handler(
    State(state): State<RuntimeHttpState>,
    Json(body): Json<IdentityDecideRequest>,
) -> Response {
    match decide_identity_access(&state, body).await {
        Ok(allowed) => (
            StatusCode::OK,
            Json(IdentityDecideResponse {
                allowed,
                reason: None,
            }),
        )
            .into_response(),
        Err(error) => error.into_response(),
    }
}

async fn decide_identity_access(
    state: &RuntimeHttpState,
    body: IdentityDecideRequest,
) -> Result<bool, IdentityDecideError> {
    let resource = parse_resource(&body.resource).map_err(IdentityDecideError::BadRequest)?;
    let permission = parse_permission(&body.permission).map_err(IdentityDecideError::BadRequest)?;

    if body.actor.trim().is_empty() {
        return Err(IdentityDecideError::BadRequest(
            "actor must not be empty".to_string(),
        ));
    }

    let api_base = gents_store::config_client::graphql_api_base(state.graphql.url())
        .map_err(|error| IdentityDecideError::Internal(error.to_string()))?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| IdentityDecideError::Internal(error.to_string()))?;
    decide_document_access(
        &client,
        &api_base,
        Some(&state.p2p_graphql),
        &DocumentAccessQuestion {
            actor: &body.actor,
            permission,
            policy_id: &resource.policy_id,
            resource_name: &resource.resource_name,
            doc_id: &resource.doc_id,
        },
    )
    .await
    .map_err(|error| match error {
        DocumentDecideError::Authorize(error) => IdentityDecideError::Internal(error.to_string()),
        DocumentDecideError::Request(error) | DocumentDecideError::ReadBody(error) => {
            IdentityDecideError::Backend(error.to_string())
        }
        DocumentDecideError::Decode(error) => IdentityDecideError::Backend(error.to_string()),
        refused @ DocumentDecideError::Refused { .. } => {
            IdentityDecideError::Backend(refused.to_string())
        }
    })
}

fn parse_resource(resource: &str) -> Result<ParsedResource, String> {
    let mut parts = resource.split('/');
    let policy_id = parts.next().unwrap_or_default();
    let resource_name = parts.next().unwrap_or_default();
    let doc_id = parts.next().unwrap_or_default();

    if parts.next().is_some()
        || policy_id.is_empty()
        || resource_name.is_empty()
        || doc_id.is_empty()
    {
        return Err("resource must have format policy_id/resource_name/doc_id".to_string());
    }

    Ok(ParsedResource {
        policy_id: policy_id.to_string(),
        resource_name: resource_name.to_string(),
        doc_id: doc_id.to_string(),
    })
}

fn parse_permission(permission: &str) -> Result<&'static str, String> {
    let permission = permission.trim();
    let suffix = permission.rsplit('.').next().unwrap_or(permission);
    if suffix.eq_ignore_ascii_case("read") {
        Ok("read")
    } else if suffix.eq_ignore_ascii_case("update") {
        Ok("update")
    } else if suffix.eq_ignore_ascii_case("delete") {
        Ok("delete")
    } else {
        Err("permission must resolve to read, update, or delete".to_string())
    }
}

impl IdentityDecideError {
    fn into_response(self) -> Response {
        let (status, reason) = match self {
            Self::BadRequest(reason) => (StatusCode::BAD_REQUEST, reason),
            Self::Backend(reason) => (StatusCode::BAD_GATEWAY, reason),
            Self::Internal(reason) => (StatusCode::INTERNAL_SERVER_ERROR, reason),
        };

        (
            status,
            Json(IdentityDecideResponse {
                allowed: false,
                reason: Some(reason),
            }),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, sync::Arc};

    use acp::{
        AcpStore, DocumentACP, DocumentPermission, Identity, LocalDocumentACP, MemoryAcpStore,
        RelationTuple, READER_RELATION,
    };
    use axum::{extract::State, routing::post, Router};
    use identity::Did;
    use serde::Deserialize;
    use serde_json::json;
    use tokio::net::TcpListener;

    use super::*;

    use gents_lean_vocab::families::agent_identity_permission_decide as decide;

    const IDENTITY_PERMISSION_POLICY_ID: &str = "identity-permission-cases";
    const IDENTITY_PERMISSION_RESOURCE_NAME: &str = "row";

    #[derive(Debug, Deserialize)]
    struct DefraDbDecisionRequest {
        actor: String,
        permission: String,
        #[serde(rename = "policyID")]
        policy_id: String,
        #[serde(rename = "resourceName")]
        resource_name: String,
        #[serde(rename = "docID")]
        doc_id: String,
    }

    async fn mock_defradb_decide(
        State(acp): State<Arc<LocalDocumentACP>>,
        identity: defra_http::ExtractIdentity,
        Json(body): Json<DefraDbDecisionRequest>,
    ) -> Response {
        assert!(
            identity.did().is_some(),
            "operator proxy must authenticate to DefraDB"
        );
        let actor = match Did::new(body.actor) {
            Ok(actor) => actor,
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": error.to_string() })),
                )
                    .into_response()
            }
        };
        let permission = match parse_acp_permission(&body.permission) {
            Ok(permission) => permission,
            Err(error) => {
                return (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))).into_response()
            }
        };
        let allowed = match acp
            .check_doc_access(
                &Identity::Authenticated(actor),
                permission,
                &body.policy_id,
                &body.resource_name,
                &body.doc_id,
            )
            .await
        {
            Ok(allowed) => allowed,
            Err(error) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": error.to_string() })),
                )
                    .into_response()
            }
        };

        (StatusCode::OK, Json(json!({ "allowed": allowed }))).into_response()
    }

    fn parse_acp_permission(permission: &str) -> Result<DocumentPermission, String> {
        match permission {
            "read" => Ok(DocumentPermission::Read),
            "update" => Ok(DocumentPermission::Update),
            "delete" => Ok(DocumentPermission::Delete),
            _ => Err(format!("invalid permission: {permission}")),
        }
    }

    async fn build_local_acp_from_lean_case(
        case: &decide::PermissionInput,
    ) -> anyhow::Result<LocalDocumentACP> {
        let store = Arc::new(MemoryAcpStore::new());
        let acp = LocalDocumentACP::new(store.clone());
        let row_owner = did_from_lean_case(&case.row_owner, case, "row_owner");

        acp.register_doc_object(
            &row_owner,
            IDENTITY_PERMISSION_POLICY_ID,
            IDENTITY_PERMISSION_RESOURCE_NAME,
            &case.row_owner,
        )
        .await?;

        let namespaced_resource =
            format!("{IDENTITY_PERMISSION_POLICY_ID}:{IDENTITY_PERMISSION_RESOURCE_NAME}");
        for grant in &case.grants {
            let principal = did_from_lean_case(&grant.actor, case, "grant.actor");
            let tuple = RelationTuple::try_new(
                principal,
                READER_RELATION,
                namespaced_resource.as_str(),
                case.row_owner.as_str(),
            )?;
            store.put_tuple(&tuple).await?;
        }

        Ok(acp)
    }

    fn did_from_lean_case(value: &str, case: &decide::PermissionInput, field: &str) -> Did {
        Did::new(value).unwrap_or_else(|error| {
            panic!(
                "case {:?}: {field} {:?} is not a valid DefraDB identity DID: {error}",
                case.name, value
            )
        })
    }

    async fn spawn_runtime_router(
        acp: LocalDocumentACP,
        owner: &gents_desktop_core::client::PrincipalIdentity,
    ) -> anyhow::Result<SocketAddr> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let (activation_runtime, activation_observation) =
            crate::http::router::empty_activation_state();
        let router = crate::http::runtime_contract_router(
            format!("http://{addr}/api/v0/graphql"),
            gents_store::config_client::GraphqlEndpoint::as_principal(
                format!("http://{addr}/api/v0/graphql"),
                owner.did(),
            )
            .with_client_signing_capability(
                gents_store::config_client::ClientSigningCapability::from_ed25519_key(
                    owner.private_key_bytes(),
                )?,
            )?,
            "identity-test".into(),
            owner.did().into(),
            "readwrite".into(),
            Ok(None),
            Vec::new(),
            None,
            None,
            None,
            None,
            None,
            crate::http::enrollment::empty_issuer_handle(),
            crate::http::enrollment::empty_decision_service_handle(),
            activation_runtime,
            activation_observation,
            Default::default(),
            Default::default(),
        )
        .contract
        .merge(
            Router::new()
                .route("/api/v0/acp/document/decide", post(mock_defradb_decide))
                .with_state(Arc::new(acp)),
        );
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok(addr)
    }

    async fn post_identity_decide(
        runtime_addr: SocketAddr,
        owner: &gents_desktop_core::client::PrincipalIdentity,
        actor: &str,
        permission: &str,
        resource: &str,
    ) -> anyhow::Result<IdentityDecideResponse> {
        let response = reqwest::Client::new()
            .post(format!("http://{runtime_addr}/identity/decide"))
            .header(
                axum::http::header::AUTHORIZATION,
                gents_store::config_client::GraphqlEndpoint::as_principal(
                    format!("http://{runtime_addr}/api/v0/graphql"),
                    owner.did(),
                )
                .with_client_signing_capability(
                    gents_store::config_client::ClientSigningCapability::from_ed25519_key(
                        owner.private_key_bytes(),
                    )?,
                )?
                .authorization()?
                .expect("operator authorization"),
            )
            .json(&json!({
                "actor": actor,
                "permission": permission,
                "resource": resource,
            }))
            .send()
            .await?;
        let status = response.status();
        let body = response.json::<IdentityDecideResponse>().await?;
        assert!(status.is_success(), "unexpected status {status}: {body:?}");
        Ok(body)
    }

    #[tokio::test]
    async fn identity_decide_endpoint_matches_resolved_lean_permission_cases() -> anyhow::Result<()>
    {
        let temp = tempfile::tempdir()?;
        let paths =
            gents_desktop_core::client::DesktopPaths::from_root(temp.path().join("operator"));
        paths.ensure_root_dirs().await?;
        let owner = gents_desktop_core::client::PrincipalIdentity::load_or_create(&paths).await?;
        for case in gents_lean_vocab::families::agent_identity_permission_decide::cases() {
            let input = &case.input;
            let runtime_addr =
                spawn_runtime_router(build_local_acp_from_lean_case(input).await?, &owner).await?;
            let resource = format!(
                "{IDENTITY_PERMISSION_POLICY_ID}/{IDENTITY_PERMISSION_RESOURCE_NAME}/{}",
                input.row_owner
            );
            let actor = post_identity_decide(
                runtime_addr,
                &owner,
                &input.requested_actor_did,
                &input.permission,
                &resource,
            )
            .await?;
            let peer = post_identity_decide(
                runtime_addr,
                &owner,
                &input.requested_peer_did,
                &input.permission,
                &resource,
            )
            .await?;
            case.check(decide::DecisionOutput {
                actor_allowed: actor.allowed,
                peer_allowed: peer.allowed,
                decisions_equal: actor.allowed == peer.allowed,
            });
        }

        Ok(())
    }
}
