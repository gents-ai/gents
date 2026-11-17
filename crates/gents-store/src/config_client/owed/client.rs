use gents_protocol::owed_settlement::{
    OwedSettlementReport, OwedSettlementRequest, OWED_SETTLEMENT_PATH,
};

use super::{continues, entry_step, EntryStep, OwedStanding, OWED_RETRY};
use crate::config_client::GraphqlEndpoint;
use crate::identity::RemoteAgentProvisionError;

/// One settlement pass, asked of the runtime under a delegation minted for
/// it at the one minting point (`GraphqlEndpoint::authorization`).
#[async_trait::async_trait]
pub trait OwedPass: Send + Sync {
    async fn pass(&self) -> Result<Vec<OwedStanding>, RemoteAgentProvisionError>;
}

/// The runtime `endpoint` serves, asked for `node_did`.
pub struct EndpointPass<'a> {
    pub endpoint: &'a GraphqlEndpoint,
    pub node_did: &'a str,
}

#[async_trait::async_trait]
impl OwedPass for EndpointPass<'_> {
    async fn pass(&self) -> Result<Vec<OwedStanding>, RemoteAgentProvisionError> {
        let report: OwedSettlementReport = crate::identity::provision_client::post_runtime(
            self.endpoint,
            OWED_SETTLEMENT_PATH,
            &OwedSettlementRequest {
                node_did: self.node_did.to_owned(),
            },
            64 * 1024,
        )
        .await?;
        Ok(report.standings)
    }
}

/// Some change the person made was not kept: the provider will never hold
/// it. The text is what the person reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{}", super::OWED_FAILED_FOR_A_PERSON)]
pub struct OwedSettlementFailed;

/// Settle what the client's own User owes while the runtime answers
/// (`Identity.OwedRegistration.continues`): one pass at a time,
/// `OWED_RETRY` apart, until nothing is left to move. A pass the runtime does
/// not answer ends this online period; the next one starts again.
pub async fn settle_owed_while_online<P>(pass: &P) -> Result<(), OwedSettlementFailed>
where
    P: OwedPass + ?Sized,
{
    let mut failed = false;
    loop {
        let standings = match pass.pass().await {
            Ok(standings) => standings,
            Err(error) => {
                tracing::debug!(%error, "owed settlement paused until the runtime answers");
                break;
            }
        };
        failed |= standings
            .iter()
            .any(|standing| entry_step(*standing) == EntryStep::Report);
        if !continues(true, &standings) {
            break;
        }
        tokio::time::sleep(OWED_RETRY).await;
    }
    if failed {
        Err(OwedSettlementFailed)
    } else {
        Ok(())
    }
}

/// Start settling in the background what the endpoint's own User owes,
/// when the endpoint mints Vera delegations; a failure is told in product
/// words. Nothing is started for any other endpoint.
pub fn spawn_settlement(
    endpoint: GraphqlEndpoint,
    node_did: String,
) -> Option<tokio::task::JoinHandle<()>> {
    endpoint.mints_delegations().then(|| {
        tokio::spawn(async move {
            let pass = EndpointPass {
                endpoint: &endpoint,
                node_did: &node_did,
            };
            if let Err(failed) = settle_owed_while_online(&pass).await {
                tracing::warn!("{failed}");
            }
        })
    })
}
