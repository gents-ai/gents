use std::time::Duration;

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::http::router::RuntimeHttpState;

#[cfg(test)]
#[path = "../../../defra-agent/src/lean_vocab_test.rs"]
mod lean_vocab_test;

#[derive(Debug, Deserialize)]
pub(crate) struct IdentityDecideRequest {
    actor: String,
    permission: String,
    resource: String,
}

#[derive(Debug, Deserialize)]
struct DefraDbDecisionResponse {
    allowed: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct IdentityDecideResponse {
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

pub(crate) async fn identity_decide_handler(
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

    let api_base = crate::graphql_access::graphql_api_base(&state.graphql)
        .map_err(|error| IdentityDecideError::Internal(error.to_string()))?;
    let url = format!("{}/acp/document/decide", api_base.trim_end_matches('/'));

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| IdentityDecideError::Internal(error.to_string()))?;
    let response = client
        .post(url)
        .json(&json!({
            "actor": body.actor,
            "permission": permission,
            "policyID": resource.policy_id,
            "resourceName": resource.resource_name,
            "docID": resource.doc_id,
        }))
        .send()
        .await
        .map_err(|error| IdentityDecideError::Backend(error.to_string()))?;

    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|error| IdentityDecideError::Backend(error.to_string()))?;
    if !status.is_success() {
        return Err(IdentityDecideError::Backend(format!(
            "DefraDB decision endpoint returned {status}: {text}"
        )));
    }

    let response: DefraDbDecisionResponse = serde_json::from_str(&text)
        .map_err(|error| IdentityDecideError::Backend(error.to_string()))?;
    Ok(response.allowed)
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
    use tokio::net::TcpListener;

    use super::*;

    use super::lean_vocab_test::{lean_identity_permission_cases, LeanIdentityPermissionCase};

    const IDENTITY_PERMISSION_POLICY_ID: &str = "identity-permission-cases";
    const IDENTITY_PERMISSION_RESOURCE_NAME: &str = "row";

    #[derive(Debug, Deserialize)]
    struct DefraDbDecisionRequest {
        actor: String,
        permission: String,
        #[serde(rename = "policyID", alias = "policy_id")]
        policy_id: String,
        #[serde(rename = "resourceName", alias = "resource_name")]
        resource_name: String,
        #[serde(rename = "docID", alias = "doc_id")]
        doc_id: String,
    }

    async fn mock_defradb_decide(
        State(acp): State<Arc<LocalDocumentACP>>,
        Json(body): Json<DefraDbDecisionRequest>,
    ) -> Response {
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
        case: &LeanIdentityPermissionCase,
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
            let principal = did_from_lean_case(&grant.principal, case, "grant.principal");
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

    fn did_from_lean_case(value: &str, case: &LeanIdentityPermissionCase, field: &str) -> Did {
        Did::new(value).unwrap_or_else(|error| {
            panic!(
                "case {:?}: {field} {:?} is not a valid Defra identity DID: {error}",
                case.name, value
            )
        })
    }

    async fn spawn_mock_defradb(acp: LocalDocumentACP) -> anyhow::Result<String> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let router = Router::new()
            .route("/api/v0/acp/document/decide", post(mock_defradb_decide))
            .with_state(Arc::new(acp));
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok(format!("http://{addr}"))
    }

    async fn spawn_runtime_router(graphql: String) -> anyhow::Result<SocketAddr> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let router = crate::http::runtime_contract_router(
            graphql,
            "identity-test-agent".to_string(),
            "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK".to_string(),
        );
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok(addr)
    }

    async fn post_identity_decide(
        runtime_addr: SocketAddr,
        actor: &str,
        permission: &str,
        resource: &str,
    ) -> anyhow::Result<IdentityDecideResponse> {
        let response = reqwest::Client::new()
            .post(format!("http://{runtime_addr}/identity/decide"))
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

    #[test]
    fn parses_resource_and_lean_style_permissions() {
        let resource = parse_resource("identity-permission-cases/row/did:key:z6Mkowner").unwrap();
        assert_eq!(resource.policy_id, "identity-permission-cases");
        assert_eq!(resource.resource_name, "row");
        assert_eq!(resource.doc_id, "did:key:z6Mkowner");

        assert_eq!(parse_permission("read").unwrap(), "read");
        assert_eq!(
            parse_permission("row:did:key:z6Mkowner:memory.read").unwrap(),
            "read"
        );
        assert!(parse_resource("identity-permission-cases/row").is_err());
        assert!(parse_permission("row:did:key:z6Mkowner:memory.admin").is_err());
    }

    #[tokio::test]
    async fn identity_decide_endpoint_matches_lean_permission_cases() -> anyhow::Result<()> {
        let cases = lean_identity_permission_cases();
        assert_eq!(
            cases.len(),
            4,
            "Lean should emit the four executable identity permission rows"
        );

        for case in cases {
            let defradb_base =
                spawn_mock_defradb(build_local_acp_from_lean_case(case).await?).await?;
            let runtime_addr =
                spawn_runtime_router(format!("{defradb_base}/api/v0/graphql")).await?;
            let resource = format!(
                "{IDENTITY_PERMISSION_POLICY_ID}/{IDENTITY_PERMISSION_RESOURCE_NAME}/{}",
                case.row_owner
            );

            let actor = post_identity_decide(
                runtime_addr,
                &case.expected_actor_principal,
                &case.permission,
                &resource,
            )
            .await?;
            let peer = post_identity_decide(
                runtime_addr,
                &case.expected_peer_principal,
                &case.permission,
                &resource,
            )
            .await?;

            assert_eq!(
                actor.allowed, case.expected_actor_allowed,
                "case {:?}: /identity/decide actor decision drifted from Lean witness",
                case.name
            );
            assert_eq!(
                peer.allowed, case.expected_peer_allowed,
                "case {:?}: /identity/decide peer decision drifted from Lean witness",
                case.name
            );
        }

        Ok(())
    }
}
