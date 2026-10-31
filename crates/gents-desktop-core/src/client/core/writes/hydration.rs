use std::collections::BTreeSet;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde::Deserialize;

use super::super::bootstrap::normalize_required;
use super::{ensure_peer_chat_ready_at, ClientCore};
use documents::{
    load_hydration_server_state, load_local_hydration_documents,
    load_local_hydration_start_evidence,
};

mod documents;

impl ClientCore {
    pub async fn ensure_session_hydration_started(
        &self,
        session_id: &str,
        node_did: &str,
    ) -> Result<(
        gents::agent::p2p_reconcile::session_hydration::ClientHydrationProgress,
        Option<String>,
    )> {
        let session_id = normalize_required("session_id", session_id)?;
        let node_did = normalize_required("node_did", node_did)?;
        let foreign_header = self
            .session_unreadable_reason(session_id, node_did)
            .is_some();
        let (progress, detail, _) = self
            .load_hydration_observation(session_id, node_did)
            .await?;
        if !should_start_session_hydration_request(&progress, session_id, node_did)
            || foreign_header
        {
            return Ok((progress, detail));
        }
        let evidence = load_local_hydration_start_evidence(
            self.node.as_ref(),
            self.node_identity.did(),
            session_id,
            node_did,
        )
        .await?;
        if !hydration_start_evidence_is_ready(false, &progress, &evidence) {
            return Ok((progress, detail));
        }
        self.admit_session_hydration_request(
            gents::agent::p2p_reconcile::session_hydration::HydrationAdmissionAction::PassiveStart,
            session_id,
            node_did,
            &progress,
            Some(&evidence),
        )
        .await?;
        self.load_hydration_status(session_id, node_did).await
    }

    pub fn transcript_node_scope(&self, node_did: &str) -> Option<String> {
        self.sync_state
            .records()
            .iter()
            .any(|peer| peer.node_did == node_did && peer.is_enrollment())
            .then(|| self.node_identity.did().to_string())
    }

    /// Whether this paired client reads the session through the node's operator
    /// access because the node owns it. The node refuses to hydrate such a
    /// session to this client's replica, so it is read only through the node.
    pub fn session_read_through_operator(&self, session_id: &str, node_did: &str) -> bool {
        let store = self.store.snapshot();
        store
            .sessions
            .iter()
            .find(|row| row.session_id == session_id && row.node_did == node_did)
            .is_some_and(|session| session.requester_did.as_deref() == Some(node_did))
            && self.transcript_node_scope(node_did).is_some()
            && self.operator_observation_graphql(node_did).is_some()
    }

    pub fn session_unreadable_reason(
        &self,
        session_id: &str,
        node_did: &str,
    ) -> Option<&'static str> {
        let store = self.store.snapshot();
        let session = store
            .sessions
            .iter()
            .find(|row| row.session_id == session_id && row.node_did == node_did)?;
        crate::client::query::session_unreadable_reason(
            session,
            self.transcript_node_scope(node_did).as_deref(),
            self.operator_graphql(node_did).is_some(),
        )
    }

    pub async fn session_hydration_progress(
        &self,
        session_id: &str,
        node_did: &str,
    ) -> Result<gents::agent::p2p_reconcile::session_hydration::ClientHydrationProgress> {
        Ok(self.session_hydration_status(session_id, node_did).await?.0)
    }

    pub async fn session_hydration_status(
        &self,
        session_id: &str,
        node_did: &str,
    ) -> Result<(
        gents::agent::p2p_reconcile::session_hydration::ClientHydrationProgress,
        Option<String>,
    )> {
        let session_id = normalize_required("session_id", session_id)?;
        let node_did = normalize_required("node_did", node_did)?;
        self.load_hydration_status(session_id, node_did).await
    }

    pub async fn retry_session_hydration(&self, session_id: &str, node_did: &str) -> Result<()> {
        let session_id = normalize_required("session_id", session_id)?;
        let node_did = normalize_required("node_did", node_did)?;
        if let Some(reason) = self.session_unreadable_reason(session_id, node_did) {
            bail!("{reason}");
        }
        let (progress, _, request) = self
            .load_hydration_observation(session_id, node_did)
            .await?;
        if !gents::agent::p2p_reconcile::session_hydration::can_admit_hydration_request(
            gents::agent::p2p_reconcile::session_hydration::HydrationAdmissionAction::ExplicitRetry,
            &request,
            &progress,
            session_id,
            node_did,
            false,
            false,
            false,
            false,
        ) {
            bail!("session hydration retry requires a failed attempt for the selected session");
        }
        if !self
            .admit_session_hydration_request(
                gents::agent::p2p_reconcile::session_hydration::HydrationAdmissionAction::ExplicitRetry,
                session_id,
                node_did,
                &progress,
                None,
            )
            .await?
        {
            bail!("session hydration retry requires a failed attempt for the selected session");
        }
        Ok(())
    }

    async fn admit_session_hydration_request(
        &self,
        action: gents::agent::p2p_reconcile::session_hydration::HydrationAdmissionAction,
        session_id: &str,
        node_did: &str,
        observed_progress: &gents::agent::p2p_reconcile::session_hydration::ClientHydrationProgress,
        evidence: Option<&LocalHydrationStartEvidence>,
    ) -> Result<bool> {
        use gents::agent::p2p_reconcile::session_hydration::HydrationAdmissionAction as Action;

        let session_id = normalize_required("session_id", session_id)?;
        let node_did = normalize_required("node_did", node_did)?;
        let evidence = match action {
            Action::PassiveStart => Some(
                evidence.context("passive hydration admission requires local start evidence")?,
            ),
            Action::ExplicitRetry => {
                anyhow::ensure!(
                    evidence.is_none(),
                    "explicit hydration retry does not accept passive start evidence"
                );
                None
            }
        };
        let peer_record = self
            .peer_record_for_chat_write(node_did, Utc::now())
            .await?;
        ensure_peer_chat_ready_at(node_did, peer_record.as_ref(), Utc::now())?;
        let request_key = format!("{}:{session_id}", self.local_peer_id());
        let expected_requester_did = self.node_identity.did().to_owned();
        let requester_did = gents::graphql::escape_graphql_string(&expected_requester_did);
        let node_did_gql = gents::graphql::escape_graphql_string(node_did);
        let session_id_gql = gents::graphql::escape_graphql_string(session_id);
        let request_key_gql = gents::graphql::escape_graphql_string(&request_key);
        let now = gents::graphql::escape_graphql_string(
            &chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        );
        let request_query = hydration_request_admission_query(&request_key_gql);
        let observed_progress = observed_progress.clone();
        let mutation = match action {
            Action::PassiveStart => format!(
                r#"mutation {{
                create_SessionHydrationRequest(input: {{
                        request_key: "{request_key_gql}",
                        requester_did: "{requester_did}",
                        node_did: "{node_did_gql}",
                        session_id: "{session_id_gql}",
                        created_at: "{now}",
                        status: "pending",
                        status_detail: "",
                        served_doc_count: 0
                }}) {{ _docID }}
            }}"#,
            ),
            Action::ExplicitRetry => format!(
                r#"mutation {{
                update_SessionHydrationRequest(
                    filter: {{
                        request_key: {{ _eq: "{request_key_gql}" }},
                        requester_did: {{ _eq: "{requester_did}" }},
                        node_did: {{ _eq: "{node_did_gql}" }},
                        session_id: {{ _eq: "{session_id_gql}" }},
                        status: {{ _eq: "rejected" }}
                    }},
                    input: {{
                        created_at: "{now}",
                        status: "pending",
                        status_detail: "",
                        served_doc_count: 0,
                        served_manifest_json: "",
                        outcome_signer_did: "",
                        outcome_signature: "",
                        processed_at: null
                    }}
                ) {{ _docID }}
            }}"#,
            ),
        };
        let operation = match action {
            Action::PassiveStart => "desktop.session_hydration.admit_initial",
            Action::ExplicitRetry => "desktop.session_hydration.retry_rejected",
        };
        gents::config_client::ConfigAccess::transact_local_write(
            &self.node,
            None,
            operation,
            move |txn| {
                let request_query = request_query.clone();
                let mutation = mutation.clone();
                let request_key = request_key.clone();
                let expected_requester_did = expected_requester_did.clone();
                let observed_progress = observed_progress.clone();
                Box::pin(async move {
                    if action == Action::PassiveStart {
                        gents::graphql::fence_arrivals(txn, "SessionHydrationRequest").await?;
                    }
                    let request_response = txn.execute(&request_query).await?;
                    let requests = transaction_rows::<HydrationAdmissionRequestRow>(
                        &request_response,
                        "SessionHydrationRequest",
                    )?;
                    anyhow::ensure!(
                        requests.len() <= 1,
                        "session hydration request key resolved to multiple rows"
                    );
                    let request = requests.first();
                    if let Some(request) = request {
                        request.ensure_target(&request_key, &expected_requester_did, node_did, session_id)?;
                    }
                    let request_state = match request {
                        None => {
                            gents::agent::p2p_reconcile::session_hydration::ClientHydrationRequestState::Missing
                        }
                        Some(request) => match request.status.as_deref() {
                            Some("pending") => {
                                gents::agent::p2p_reconcile::session_hydration::ClientHydrationRequestState::Pending
                            }
                            Some("served") => {
                                gents::agent::p2p_reconcile::session_hydration::ClientHydrationRequestState::Served(BTreeSet::new())
                            }
                            Some("rejected") => {
                                gents::agent::p2p_reconcile::session_hydration::ClientHydrationRequestState::Rejected(None)
                            }
                            Some(_) | None if action == Action::PassiveStart => {
                                gents::agent::p2p_reconcile::session_hydration::ClientHydrationRequestState::Pending
                            }
                            Some(status) => anyhow::bail!("unknown session hydration request status {status:?}"),
                            None => anyhow::bail!("session hydration request is missing status"),
                        },
                    };
                    let evidence = evidence.as_ref();
                    if !gents::agent::p2p_reconcile::session_hydration::can_admit_hydration_request(
                        action,
                        &request_state,
                        &observed_progress,
                        session_id,
                        node_did,
                        false,
                        evidence.is_some_and(|evidence| evidence.owned_session_present),
                        observed_progress.merged_count > 0,
                        evidence.is_some_and(|evidence| evidence.nonterminal_request_present),
                    ) {
                        return Ok(false);
                    }
                    let response = txn.execute(&mutation).await?;
                    let field = match action {
                        Action::PassiveStart => "create_SessionHydrationRequest",
                        Action::ExplicitRetry => "update_SessionHydrationRequest",
                    };
                    let rows = gents::graphql::mutation_documents(&response, field)?;
                    let changed = rows.is_some_and(|rows| !rows.is_empty());
                    if action == Action::PassiveStart {
                        anyhow::ensure!(
                            changed,
                            "initial session hydration request create returned no document"
                        );
                    }
                    Ok(changed)
                })
            },
        )
        .await
    }

    async fn load_hydration_status(
        &self,
        session_id: &str,
        node_did: &str,
    ) -> Result<(
        gents::agent::p2p_reconcile::session_hydration::ClientHydrationProgress,
        Option<String>,
    )> {
        let (progress, detail, _) = self
            .load_hydration_observation(session_id, node_did)
            .await?;
        Ok((progress, detail))
    }

    async fn load_hydration_observation(
        &self,
        session_id: &str,
        node_did: &str,
    ) -> Result<(
        gents::agent::p2p_reconcile::session_hydration::ClientHydrationProgress,
        Option<String>,
        gents::agent::p2p_reconcile::session_hydration::ClientHydrationRequestState,
    )> {
        let (request, detail) = load_hydration_server_state(
            self.node.as_ref(),
            self.local_peer_id(),
            self.node_identity.did(),
            session_id,
            node_did,
            &self.node_identity,
        )
        .await?;
        use gents::agent::p2p_reconcile::session_hydration::ClientHydrationRequestState;
        let verified_manifest = match &request {
            ClientHydrationRequestState::Served(documents)
            | ClientHydrationRequestState::Rejected(Some(documents)) => Some(documents),
            _ => None,
        };
        let merged = load_local_hydration_documents(
            self.node.as_ref(),
            self.node_identity.did(),
            session_id,
            node_did,
            verified_manifest,
        )
        .await?;
        Ok((
            gents::agent::p2p_reconcile::session_hydration::project_durable_hydration_progress(
                session_id,
                node_did,
                merged,
                request.clone(),
            ),
            detail,
            request,
        ))
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct LocalHydrationStartEvidence {
    owned_session_present: bool,
    nonterminal_request_present: bool,
}

#[derive(Deserialize)]
struct HydrationAdmissionRequestRow {
    request_key: Option<String>,
    requester_did: Option<String>,
    node_did: Option<String>,
    session_id: Option<String>,
    status: Option<String>,
}

impl HydrationAdmissionRequestRow {
    fn ensure_target(
        &self,
        request_key: &str,
        requester_did: &str,
        node_did: &str,
        session_id: &str,
    ) -> Result<()> {
        for (field, actual, expected) in [
            ("request_key", self.request_key.as_deref(), request_key),
            (
                "requester_did",
                self.requester_did.as_deref(),
                requester_did,
            ),
            ("node_did", self.node_did.as_deref(), node_did),
            ("session_id", self.session_id.as_deref(), session_id),
        ] {
            anyhow::ensure!(
                actual == Some(expected),
                "session hydration request {field} does not match the selected target"
            );
        }
        Ok(())
    }
}

fn transaction_rows<T>(response: &serde_json::Value, field: &str) -> Result<Vec<T>>
where
    T: serde::de::DeserializeOwned,
{
    let data = response
        .get("data")
        .and_then(serde_json::Value::as_object)
        .context("hydration admission transaction omitted response data")?;
    let rows = data
        .get(field)
        .with_context(|| format!("hydration admission transaction omitted {field}"))?;
    Vec::<T>::deserialize(rows).with_context(|| format!("decode {field} transaction rows"))
}

fn hydration_request_admission_query(request_key_gql: &str) -> String {
    format!(
        r#"{{ SessionHydrationRequest(filter: {{ request_key: {{ _eq: "{request_key_gql}" }} }}) {{
            request_key requester_did node_did session_id status
        }} }}"#
    )
}

fn should_start_session_hydration_request(
    progress: &gents::agent::p2p_reconcile::session_hydration::ClientHydrationProgress,
    session_id: &str,
    node_did: &str,
) -> bool {
    use gents::agent::p2p_reconcile::session_hydration::ClientHydrationPhase;
    progress.session_id == session_id
        && progress.node_did == node_did
        && progress.phase == ClientHydrationPhase::Idle
}

fn hydration_start_evidence_is_ready(
    foreign_header: bool,
    progress: &gents::agent::p2p_reconcile::session_hydration::ClientHydrationProgress,
    evidence: &LocalHydrationStartEvidence,
) -> bool {
    gents::agent::p2p_reconcile::session_hydration::can_start_hydration(
        foreign_header,
        evidence.owned_session_present,
        progress.merged_count > 0,
        evidence.nonterminal_request_present,
    )
}
