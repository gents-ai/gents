use serde::{Deserialize, Serialize};

use super::ConfigAccess;

/// Runtime code paths classified by the authority they must act under
/// (Lean `Identity.RuntimeActorAuthority.RuntimePath`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimePath {
    NodeOperational,
    RuntimeConfigUse,
    SessionResume,
    SessionSelection,
    RequestWatch,
    WakeInputLoad,
}

/// Authority class required by a [`RuntimePath`]
/// (Lean `Identity.RuntimeActorAuthority.RequiredAuthority`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequiredAuthority {
    NodeKey,
    SelectedAgent,
}

/// Principal presented at a runtime entry point
/// (Lean `Identity.RuntimeActorAuthority.ActingPrincipal`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActingPrincipal {
    NodeKey,
    SelectedAgent,
    OtherActor,
    Absent,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RuntimeActorAuthorityError {
    #[error("runtime path {path:?} requires the selected Agent and refuses {actor:?}")]
    SelectedAgentRequired {
        path: RuntimePath,
        actor: ActingPrincipal,
    },
    #[error("runtime path {path:?} requires the hosting runtime node key and refuses {actor:?}")]
    NodeKeyRequired {
        path: RuntimePath,
        actor: ActingPrincipal,
    },
}

/// Classifies an erased error as a [`RuntimeActorAuthorityError`].
pub fn is_runtime_actor_authority_error(error: &(dyn std::error::Error + 'static)) -> bool {
    gents_protocol::error_chain::find_map_source(error, |cause| {
        cause.downcast_ref::<RuntimeActorAuthorityError>()
    })
    .is_some()
}

/// Authority class required for `path`
/// (Lean `Identity.RuntimeActorAuthority.requiredAuthority`).
pub fn required_authority(path: RuntimePath) -> RequiredAuthority {
    match path {
        RuntimePath::NodeOperational | RuntimePath::RuntimeConfigUse => RequiredAuthority::NodeKey,
        RuntimePath::SessionResume
        | RuntimePath::SessionSelection
        | RuntimePath::RequestWatch
        | RuntimePath::WakeInputLoad => RequiredAuthority::SelectedAgent,
    }
}

/// Whether `path` admits `actor` before executing any store read or write
/// (Lean `Identity.RuntimeActorAuthority.actorAdmitted`).
pub fn actor_admitted(path: RuntimePath, actor: ActingPrincipal) -> bool {
    match (required_authority(path), actor) {
        (RequiredAuthority::NodeKey, ActingPrincipal::NodeKey) => true,
        (RequiredAuthority::NodeKey, _) => false,
        (
            RequiredAuthority::SelectedAgent,
            ActingPrincipal::SelectedAgent | ActingPrincipal::OtherActor,
        ) => true,
        (RequiredAuthority::SelectedAgent, ActingPrincipal::NodeKey | ActingPrincipal::Absent) => {
            false
        }
    }
}

/// Enforces Lean `Identity.RuntimeActorAuthority.actorAdmitted` for `(path, actor)`.
pub fn check_runtime_actor_authority(
    path: RuntimePath,
    actor: ActingPrincipal,
) -> std::result::Result<(), RuntimeActorAuthorityError> {
    if actor_admitted(path, actor) {
        return Ok(());
    }
    Err(match required_authority(path) {
        RequiredAuthority::SelectedAgent => {
            RuntimeActorAuthorityError::SelectedAgentRequired { path, actor }
        }
        RequiredAuthority::NodeKey => RuntimeActorAuthorityError::NodeKeyRequired { path, actor },
    })
}

/// Classifies a concrete actor DID relative to the hosting node and `node_did`.
/// A non-empty DID distinct from both `node_did` and `node.node_identity_did()`
/// is admitted to selected-Agent paths so document ACP decides visibility
/// without a DAC bypass (`Identity.Household.without_bypass_document_acp_decides`).
pub fn classify_acting_principal(
    node: Option<&defra_node::Node>,
    node_did: &str,
    actor: Option<&str>,
) -> ActingPrincipal {
    let Some(actor) = actor.map(str::trim).filter(|did| !did.is_empty()) else {
        return ActingPrincipal::Absent;
    };
    if actor == node_did || node.and_then(defra_node::Node::node_identity_did) == Some(actor) {
        return ActingPrincipal::NodeKey;
    }
    ActingPrincipal::SelectedAgent
}

/// Enforces [`check_runtime_actor_authority`] for an embedded node entry point.
pub fn ensure_runtime_path_actor(
    node: &defra_node::Node,
    node_did: &str,
    path: RuntimePath,
    actor: Option<&str>,
) -> std::result::Result<(), RuntimeActorAuthorityError> {
    check_runtime_actor_authority(path, classify_acting_principal(Some(node), node_did, actor))
}

/// Enforces [`check_runtime_actor_authority`] for a [`ConfigAccess`] entry point.
pub fn ensure_access_for_runtime_path(
    access: &ConfigAccess,
    node_did: &str,
    path: RuntimePath,
) -> std::result::Result<(), RuntimeActorAuthorityError> {
    let actor = access.actor_did();
    let node = access.local_node().map(std::sync::Arc::as_ref);
    check_runtime_actor_authority(
        path,
        classify_acting_principal(node, node_did, actor.as_deref()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_authority_matches_lean() {
        gents_lean_vocab::families::identity_runtime_actor_authority_required_authority::replay(
            |input| required_authority(input.runtime_path),
        );
    }

    #[test]
    fn actor_admitted_matches_lean() {
        gents_lean_vocab::families::identity_runtime_actor_authority_actor_admitted::replay(
            |input| check_runtime_actor_authority(input.path, input.actor).is_ok(),
        );
    }
}
