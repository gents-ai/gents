use std::sync::Arc;
use std::time::Duration;

use defra_node::Node as EmbeddedNode;
pub use gents_protocol::owed_settlement::GrantWait;
use gents_protocol::owed_settlement::{OwedAwaitReport, OwedAwaitRequest, OWED_AWAIT_PATH};
use query::TransactionHandle;

use crate::config_client::{ConfigClientError, GraphqlEndpoint};

/// `Identity.OwedRegistration.ownGrantDeadlineSeconds`.
pub const OWN_GRANT_DEADLINE: Duration = Duration::from_secs(10);

/// `Identity.OwedRegistration.DependentStep`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependentStep {
    Proceed,
    Report,
    Wait,
}

/// `Identity.OwedRegistration.dependentStep`.
pub fn dependent_step(wait: GrantWait) -> DependentStep {
    match wait {
        GrantWait::Settled => DependentStep::Proceed,
        GrantWait::Parked => DependentStep::Report,
        GrantWait::Pending => DependentStep::Wait,
    }
}

/// `Identity.OwedRegistration.observedGrant`.
pub fn observed_grant(external_provider: bool, provider: GrantWait) -> GrantWait {
    match external_provider {
        true => provider,
        false => GrantWait::Settled,
    }
}

/// What lets a committed write wait for the access changes its own
/// transaction owes: the node, the transaction and the actor it ran as, or,
/// for a transaction a client opened over HTTP, its runtime and transaction.
#[derive(Clone)]
pub enum OwedAccessTicket {
    Embedded {
        node: Arc<EmbeddedNode>,
        handle: TransactionHandle,
        actor: Option<String>,
    },
    Http {
        endpoint: GraphqlEndpoint,
        node_did: String,
        transaction: String,
    },
    /// A transaction on a home on the node's own store: its access changes
    /// applied with it (`observedGrant false`).
    NothingOwed,
}

impl OwedAccessTicket {
    /// Wait, at most `deadline`, until the provider holds what the
    /// transaction owes, or nothing more will be sent, and say which. The
    /// node wakes the wait when an owed record changes.
    pub async fn wait(&self, deadline: Duration) -> Result<GrantWait, ConfigClientError> {
        match self {
            Self::Embedded {
                node,
                handle,
                actor,
            } => {
                let context = defra_core::RequestContext::default().with_identity(actor.clone());
                await_on(node, &context, handle, deadline).await
            }
            Self::NothingOwed => Ok(GrantWait::Settled),
            Self::Http {
                endpoint,
                node_did,
                transaction,
            } => {
                let report: OwedAwaitReport = crate::identity::provision_client::post_runtime(
                    endpoint,
                    OWED_AWAIT_PATH,
                    &OwedAwaitRequest {
                        node_did: node_did.clone(),
                        transaction: transaction.clone(),
                    },
                    4 * 1024,
                )
                .await
                .map_err(|error| ConfigClientError::OwedAwait(Box::new(error)))?;
                Ok(report.wait)
            }
        }
    }

    /// [`Self::wait`] with [`OWN_GRANT_DEADLINE`], decided
    /// (`Identity.OwedRegistration.dependentStep`).
    pub async fn dependent_step(&self) -> Result<DependentStep, ConfigClientError> {
        Ok(dependent_step(self.wait(OWN_GRANT_DEADLINE).await?))
    }
}

/// Why the step that depends on a write's own access changes did not run.
#[derive(Debug, thiserror::Error)]
pub enum OwnGrantError {
    #[error("{}", super::OWED_FAILED_FOR_A_PERSON)]
    NotKept,
    #[error("This change is saved and its access is still being set up on this home. Try again in a moment.")]
    StillWaiting,
    #[error(fmt = gents_protocol::error_chain::held)]
    Unanswered(#[from] ConfigClientError),
}

impl OwedAccessTicket {
    /// Run nothing further unless the write's own access changes settled
    /// (`Identity.OwedRegistration.a_dependent_step_runs_only_on_settled_grants`).
    pub async fn require_settled(&self) -> Result<(), OwnGrantError> {
        match self.dependent_step().await? {
            DependentStep::Proceed => Ok(()),
            DependentStep::Report => Err(OwnGrantError::NotKept),
            DependentStep::Wait => Err(OwnGrantError::StillWaiting),
        }
    }
}

/// Serve a client's wait for its own transaction's owed access changes, as
/// the caller `context` authenticates.
pub async fn serve_await(
    node: &EmbeddedNode,
    context: &defra_core::RequestContext,
    transaction: &str,
) -> Result<GrantWait, ConfigClientError> {
    let handle: TransactionHandle = transaction
        .parse()
        .map_err(|_| ConfigClientError::OwedAwaitTransaction)?;
    await_on(node, context, &handle, OWN_GRANT_DEADLINE).await
}

async fn await_on(
    node: &EmbeddedNode,
    context: &defra_core::RequestContext,
    handle: &TransactionHandle,
    deadline: Duration,
) -> Result<GrantWait, ConfigClientError> {
    let external = crate::local::document_acp::node_delegation_audience(node).is_some();
    let provider = match node
        .await_owed_acp_as(context, handle, deadline)
        .await
        .map_err(|error| ConfigClientError::Node(Box::new(error)))?
    {
        acp::OwedWait::Settled => GrantWait::Settled,
        acp::OwedWait::Parked(_) => GrantWait::Parked,
        acp::OwedWait::Pending { .. } => GrantWait::Pending,
    };
    Ok(observed_grant(external, provider))
}
