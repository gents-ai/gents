use super::OwnerStagedRoute;
use crate::config_client::{ConfigAccess, ConfigClientError};
use crate::node_scope::ContextCalls as _;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use defra_http::ExtractIdentity;
#[derive(serde::Deserialize)]
pub(super) struct AdministratorGrant {
    doc_id: String,
    target_actor: String,
}

pub(super) async fn grant_administrator(
    State(route): State<OwnerStagedRoute>,
    headers: HeaderMap,
    actor: ExtractIdentity,
    Json(grant): Json<AdministratorGrant>,
) -> StatusCode {
    use crate::config_client::client_authentication::ClientAuthorization;
    let Ok(caller) = ClientAuthorization::from_http(&actor, &headers) else {
        return StatusCode::UNAUTHORIZED;
    };
    let Some((node, _)) = route.node() else {
        return StatusCode::SERVICE_UNAVAILABLE;
    };
    let Ok(member) = identity::Did::new(&grant.target_actor) else {
        return StatusCode::BAD_REQUEST;
    };
    let access =
        ConfigAccess::local_as(node, caller.actor().clone()).with_client_authorization(caller);
    let Ok(access) = access else {
        return StatusCode::UNAUTHORIZED;
    };
    match access
        .add_actor_relationship(
            "Node",
            &grant.doc_id,
            crate::identity::NODE_ADMINISTRATOR_RELATION,
            &member,
        )
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(error) if error.is_permission_denied() => StatusCode::FORBIDDEN,
        Err(error) => {
            tracing::warn!(%error, "granting Node administration failed");
            StatusCode::SERVICE_UNAVAILABLE
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct PermissionGrant {
    relation: String,
    target_actor: String,
}

pub(super) async fn grant_permission(
    State(route): State<OwnerStagedRoute>,
    actor: ExtractIdentity,
    Json(grant): Json<PermissionGrant>,
) -> StatusCode {
    if actor.did().is_none() {
        return StatusCode::UNAUTHORIZED;
    }
    let Some((node, _)) = route.node() else {
        return StatusCode::SERVICE_UNAVAILABLE;
    };
    let Ok(member) = identity::Did::new(grant.target_actor) else {
        return StatusCode::BAD_REQUEST;
    };
    let _guard = match crate::config_client::node_permissions::admit_permission_grant(
        &node, &member,
    )
    .await
    {
        Ok(guard) => guard,
        Err(ConfigClientError::AdministrationRemovalPending { .. }) => return StatusCode::CONFLICT,
        Err(error) => {
            tracing::warn!(%error, "checking enrollment permission admission failed");
            return StatusCode::SERVICE_UNAVAILABLE;
        }
    };
    match node
        .add_nac_actor_relationship_as(&actor.context(), &grant.relation, member.as_str())
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT,
        Err(error) => {
            let error = ConfigClientError::from(error);
            if error.is_permission_denied() {
                StatusCode::FORBIDDEN
            } else {
                tracing::warn!(%error, "granting node permission failed");
                StatusCode::SERVICE_UNAVAILABLE
            }
        }
    }
}
