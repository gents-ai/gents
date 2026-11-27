use std::collections::BTreeMap;
use std::sync::{Arc, Mutex as StdMutex};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::Node as EmbeddedNode;
use gents_store::graphql::rows;
use gents_store::identity::NodeIdentity;
use serde_json::Value;
use tokio::sync::Mutex;

mod decision;
mod delivery;
mod graphql;
mod project;
mod projection;
mod renewal;
mod verified;

pub use decision::EnrollmentDecisionOutcome;
pub(super) use graphql::RequestRow;
pub use projection::{
    ActiveEnrollment, DeniedEnrollment, EnrollmentDecisionRefusal, EnrollmentProjection,
    PendingEnrollment, RevokedEnrollment,
};

use graphql::{NetworkRow, ENROLLMENT_DOCUMENT_QUERY};

#[derive(Clone)]
pub struct GraphqlEnrollmentStore {
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn NodeIdentity>,
    decision_lock: Arc<Mutex<()>>,
    fail_closed: Arc<FailClosedLog>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FailClosed {
    NoNetwork,
    RootRows(String),
    Authority(String),
}

#[derive(Debug, Default)]
struct FailClosedLog(StdMutex<Option<FailClosed>>);

impl FailClosedLog {
    fn record(&self, state: Option<FailClosed>) {
        let mut last = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *last == state {
            return;
        }
        match &state {
            None => tracing::info!("enrollment authority projection recovered"),
            Some(FailClosed::NoNetwork) => {
                tracing::debug!("enrollment authority fail closed: no Network configured")
            }
            Some(FailClosed::RootRows(error)) => {
                tracing::warn!(error = %error, "enrollment root authority projected fail closed")
            }
            Some(FailClosed::Authority(error)) => {
                tracing::warn!(error = %error, "enrollment authority projected fail closed")
            }
        }
        *last = state;
    }
}

impl GraphqlEnrollmentStore {
    pub fn new(node: Arc<EmbeddedNode>, identity: Arc<dyn NodeIdentity>) -> Self {
        let decision_lock = gents_store::identity::enrollment_decision_gate(&node);
        Self {
            node,
            identity,
            decision_lock,
            fail_closed: Arc::default(),
        }
    }

    /// The enrollment decision lock, which every revision writer on this node
    /// holds through its commit (Lean `Enrollment.RouteFence`). A home admits
    /// one runtime process, so holding it orders a write against every
    /// revision this node writes.
    pub(super) async fn decision_guard(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.decision_lock.lock().await
    }

    fn runtime_access(&self) -> Result<gents_store::config_client::ConfigAccess> {
        Ok(gents_store::config_client::ConfigAccess::local_as(
            self.node.clone(),
            identity::Did::new(self.identity.did())?,
        ))
    }

    pub(super) async fn publish_records(
        &self,
        operation: &'static str,
        mutation: &str,
        collections: &[&str],
        recipient: &str,
        withdrawal: Option<&gents_store::identity::EnrollmentGrantWithdrawal>,
    ) -> Result<Value> {
        use gents_store::config_client::ConfigAccess;
        use gents_store::graphql::escape_graphql_string;

        anyhow::ensure!(
            self.node.node_identity_did() == Some(self.identity.did()),
            "enrollment publication requires the local runtime identity"
        );
        gents_store::local::document_acp::ensure_node_publication(&self.node).await?;
        let runtime = identity::Did::new(self.identity.did())?;
        let reader = identity::Did::new(recipient)?;
        let access = ConfigAccess::local_as(self.node.clone(), runtime.clone());
        access
            .transact_write(operation, |txn| {
                let runtime = &runtime;
                let reader = &reader;
                Box::pin(async move {
                    txn.validate_runtime_actor(runtime)?;
                    let query = format!(
                        r#"{{ Node(filter: {{node_did: {{_eq: "{}"}}}}, limit: 2) {{_docID}} }}"#,
                        escape_graphql_string(runtime.as_str())
                    );
                    let nodes = txn.execute(&query).await?;
                    let nodes = gents_protocol::graphql::graphql_rows_from_response(&nodes, "Node");
                    let [parent] = nodes.as_slice() else {
                        anyhow::bail!(
                            "enrollment publication requires exactly one visible local Node"
                        );
                    };
                    let parent = parent["_docID"]
                        .as_str()
                        .filter(|id| !id.is_empty())
                        .context("enrollment publication Node has no document ID")?;
                    let owners = txn.document_owners("Node", &[parent.to_owned()]).await?;
                    anyhow::ensure!(
                        owners.len() == 1 && owners[0].visible && owners[0].owner.is_some(),
                        "enrollment publication Node has no visible native owner"
                    );
                    let response = txn.execute(mutation).await?;
                    for collection in collections {
                        let doc = gents_protocol::graphql::extract_mutation_doc_id(
                            &response, collection,
                        )?;
                        txn.add_resource_relationship(collection, &doc, "parent", "nodes", parent)
                            .await?;
                        txn.add_actor_relationship(collection, &doc, "reader", reader)
                            .await?;
                    }
                    if let Some(withdrawal) = withdrawal {
                        withdrawal.retire_owing_in_txn(txn).await?;
                    }
                    Ok::<_, anyhow::Error>(response)
                })
            })
            .await
    }

    pub async fn load_projection(&self) -> Result<EnrollmentProjection> {
        let response = gents_store::graphql::graphql_for_decision(
            &self.node,
            ENROLLMENT_DOCUMENT_QUERY,
            "query authenticated enrollment documents",
        )
        .await?;
        self.project_loaded_response(&response).await
    }

    pub(crate) async fn load_projection_in_txn(
        &self,
        txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    ) -> Result<EnrollmentProjection> {
        let response = txn
            .execute_local_response(ENROLLMENT_DOCUMENT_QUERY)
            .await?;
        self.project_loaded_response(&response).await
    }

    async fn project_loaded_response(
        &self,
        response: &query::QueryResponse,
    ) -> Result<EnrollmentProjection> {
        let now = Utc::now();
        let raw_network_rows = rows::<Value>(response, "Network")?;
        let network_id = raw_network_rows
            .first()
            .and_then(|row| row.get("network_id"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let network_rows = raw_network_rows
            .into_iter()
            .map(serde_json::from_value::<NetworkRow>)
            .collect::<std::result::Result<Vec<_>, _>>();
        let network_rows = match network_rows {
            Ok(rows) => rows,
            Err(error) => {
                self.fail_closed
                    .record(Some(FailClosed::RootRows(error.to_string())));
                return Ok(EnrollmentProjection::conflicted(network_id, error));
            }
        };
        match self.project_response(response, &network_rows, now).await {
            Ok(projection) => {
                self.fail_closed.record(None);
                Ok(projection)
            }
            Err(error) => {
                self.fail_closed.record(Some(if network_rows.is_empty() {
                    FailClosed::NoNetwork
                } else {
                    FailClosed::Authority(format!("{error:#}"))
                }));
                Ok(EnrollmentProjection::conflicted(network_id, error))
            }
        }
    }

    async fn project_response(
        &self,
        response: &query::QueryResponse,
        network_rows: &[NetworkRow],
        now: DateTime<Utc>,
    ) -> Result<EnrollmentProjection> {
        let network = self.verified_network(network_rows).await?;
        let project::VerifiedRequests {
            requests,
            request_scopes,
            mut findings,
        } = self.verify_requests(response, &network).await?;
        let requests_by_id = requests.iter().fold(BTreeMap::new(), |mut map, verified| {
            map.entry(verified.record.request_id.clone())
                .or_insert_with(Vec::new)
                .push(verified);
            map
        });
        let reused_challenge_requests = project::flag_request_identity_conflicts(
            &requests,
            &requests_by_id,
            &mut findings.request_conflicts,
        );
        let index = project::RequestIndex {
            network: &network,
            request_scopes: &request_scopes,
            requests_by_id: &requests_by_id,
        };
        let decisions = self
            .verify_decisions(response, index, &mut findings)
            .await?;
        let decisions_by_request = decisions.iter().fold(BTreeMap::new(), |mut map, verified| {
            map.entry(verified.record.request_id.clone())
                .or_insert_with(Vec::new)
                .push(verified);
            map
        });
        project::flag_duplicate_decisions(&decisions_by_request, &mut findings.request_conflicts);
        let revisions = self
            .verify_revisions(response, index, &decisions_by_request, &mut findings)
            .await?;
        let valid_revision_max = project::flag_current_invalid_authority(&revisions, &mut findings);
        let receipts = self
            .verify_receipts(response, index, &decisions_by_request, &mut findings)
            .await?;
        let documents = project::VerifiedDocuments {
            requests: &requests,
            decisions: &decisions,
            revisions: &revisions,
            receipts: &receipts,
            decisions_by_request: &decisions_by_request,
        };
        let durable = project::durable_documents(
            &network,
            now,
            documents,
            &valid_revision_max,
            &mut findings,
        );
        let mut projection = EnrollmentProjection {
            network_id: Some(network.network_id.clone()),
            next_authorization_sequences: std::mem::take(&mut findings.observed_revision_next),
            ..EnrollmentProjection::default()
        };
        project::project_decided(now, documents, &durable, &findings, &mut projection)?;
        project::project_revoked(documents, &findings, &mut projection);
        Ok(project::finish_projection(
            projection,
            findings,
            reused_challenge_requests,
        ))
    }
}
