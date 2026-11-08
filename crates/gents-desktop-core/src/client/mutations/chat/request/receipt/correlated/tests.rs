use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use gents::store::identity::{KeyIdentity, NodeIdentity};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use serde_json::{json, Value};
use uuid::Uuid;

use super::*;

struct Facts {
    root: tempfile::TempDir,
    nodes: BTreeMap<u64, KeyIdentity>,
    node_dids: BTreeMap<u64, String>,
    client_keys: BTreeMap<u64, KeyIdentity>,
    bound_clients: BTreeMap<u64, String>,
    clients: BTreeMap<String, u64>,
    envelopes: BTreeMap<String, u64>,
    payloads: BTreeMap<String, u64>,
}

impl Facts {
    fn new() -> Result<Self> {
        Ok(Self {
            root: tempfile::tempdir()?,
            nodes: BTreeMap::new(),
            node_dids: BTreeMap::new(),
            client_keys: BTreeMap::new(),
            bound_clients: BTreeMap::new(),
            clients: BTreeMap::new(),
            envelopes: BTreeMap::new(),
            payloads: BTreeMap::new(),
        })
    }

    /// Binds a modeled node or client to a served runtime's real identity.
    fn bind_node(&mut self, value: u64, did: &str) {
        self.node_dids.insert(value, did.to_owned());
    }

    fn bind_client(&mut self, value: u64, did: &str) {
        self.bound_clients.insert(value, did.to_owned());
        self.clients.insert(did.to_owned(), value);
    }

    fn bind_payload(&mut self, pending: &UnresolvedSubmission, value: u64) {
        self.payloads
            .insert(digest(pending.mutation.as_bytes()), value);
    }

    fn node(&mut self, value: u64) -> Result<String> {
        if let Some(did) = self.node_dids.get(&value) {
            return Ok(did.clone());
        }
        if !self.nodes.contains_key(&value) {
            self.nodes.insert(
                value,
                KeyIdentity::load_or_create(
                    self.root.path().join(format!("fixture-{value}.key")),
                    None,
                )?,
            );
        }
        Ok(self.nodes[&value].did().to_owned())
    }

    /// The modeled requester's client key: a desktop journal is written by
    /// the client whose key is its local node identity.
    fn client(&mut self, value: u64) -> Result<String> {
        if let Some(did) = self.bound_clients.get(&value) {
            return Ok(did.clone());
        }
        if !self.client_keys.contains_key(&value) {
            self.client_keys.insert(
                value,
                KeyIdentity::load_or_create(
                    self.root.path().join(format!("client-{value}.key")),
                    None,
                )?,
            );
        }
        let did = self.client_keys[&value].did().to_owned();
        self.clients.insert(did.clone(), value);
        Ok(did)
    }

    fn intent(&mut self, input: &Value) -> Result<Intent> {
        let envelope = number(input, "envelope")?;
        let client = self.client(number(&input["caller"], "requester")?)?;
        let intent = Intent::new(
            client,
            self.node(number(input, "node")?)?,
            identity(number(&input["caller"], "request")?),
            format!("authored-envelope-{envelope}").as_bytes(),
        );
        self.clients.insert(
            intent.client_did.clone(),
            number(&input["caller"], "requester")?,
        );
        self.envelopes
            .insert(intent.envelope_hash.clone(), envelope);
        Ok(intent)
    }

    async fn pending(&mut self, input: &Value, context: &Value) -> Result<UnresolvedSubmission> {
        let node_id = number(context, "node")?;
        let node = self.node(node_id)?;
        let request_id = identity(number(input, "request")?);
        let session = identity(number(input, "session")?);
        let salt = number(input, "signedBytes")?;
        let mut create = AgentRequestCreate::base(
            RequestPurpose::Normal,
            &request_id,
            &node,
            &node,
            "fixture-agent",
            &session,
            format!("envelope-{}", number(context, "envelope")?),
            "interactive",
            format!("2025-01-01T00:{:02}:{:02}Z", (salt / 60) % 60, salt % 60),
            AgentRequestAdmissionRecord::local_self(&node),
        );
        gents::sign_agent_request_create(&self.nodes[&node_id], &mut create).await?;
        let pending = UnresolvedSubmission::new(
            create,
            SubmittedRequest {
                request_id,
                session_id: session,
                node_did: node,
                agent_did: Some("fixture-agent".into()),
            },
        )?;
        self.payloads
            .insert(digest(pending.mutation.as_bytes()), salt);
        Ok(pending)
    }

    async fn proposal(&mut self, intent: &Value, prepared: &Value) -> Result<Record> {
        let binding = self.intent(intent)?;
        let pending = self.pending(prepared, intent).await?;
        Ok(Record::proposal(binding, pending))
    }

    async fn reference(&mut self, prepared: &Value, context: &Value) -> Result<Reference> {
        Ok(self.proposal(context, prepared).await?.reference)
    }

    fn reference_json(&self, reference: &Reference) -> Result<Value> {
        Ok(json!({
            "session": Uuid::parse_str(&reference.request.session_id)?.as_u128(),
            "request": Uuid::parse_str(&reference.request.request_id)?.as_u128(),
            "signedBytes":self.payloads.get(&reference.signed_payload_hash).context("known signed payload")?,
        }))
    }

    fn records_json(&self, records: &[Record]) -> Result<Value> {
        records.iter().map(|record| {
            let node = self.nodes.iter().find(|(_, identity)| identity.did() == record.intent.node_did)
                .map(|(id, _)| *id)
                .or_else(|| self.node_dids.iter().find(|(_, did)| **did == record.intent.node_did).map(|(id, _)| *id))
                .context("known native node")?;
            Ok(json!({
                "intent": {
                    "caller": { "requester": self.clients.get(&record.intent.client_did).context("caller")?,
                        "request": Uuid::parse_str(&record.intent.request_id)?.as_u128() },
                    "node":node, "envelope":self.envelopes.get(&record.intent.envelope_hash).context("known envelope")?,
                },
                "prepared":self.reference_json(&record.reference)?,
                "outcome":record.outcome,
                "retainedBytes":record.pending.is_some(),
                "acknowledged":record.acknowledged,
            }))
        }).collect::<Result<Vec<_>>>().map(Value::from)
    }

    fn decision_json(&self, decision: Decision) -> Result<Value> {
        Ok(match decision {
            Decision::Refused => json!({"kind":"refused"}),
            Decision::Reconcile(reference) => {
                json!({"kind":"reconcile", "prepared":self.reference_json(&reference)?})
            }
            Decision::Settled(reference, outcome) => {
                json!({"kind":"settled", "prepared":self.reference_json(&reference)?, "outcome":outcome})
            }
        })
    }
}

fn number(value: &Value, key: &str) -> Result<u64> {
    value[key]
        .as_u64()
        .with_context(|| format!("modeled {key}"))
}
fn identity(value: u64) -> String {
    Uuid::from_u128(u128::from(value)).to_string()
}

/// Only serialized identity/byte facts and a retained storage vector live here.
/// Decisions run through the production receipt owner; all expected transitions
/// are emitted by the executable model, including malformed initial records.
fn model_cases() -> Result<Vec<Value>> {
    let output = gents_lean_contract::run_prepared_generator(
        &gents_lean_contract::proofs_dir()?,
        "Proofs.Conformance.ClientSubmissionReceipt:olean",
        "Proofs/Conformance/ClientSubmissionReceipt.lean",
    )?;
    let cases: Vec<Value> = serde_json::from_str(&output)?;
    anyhow::ensure!(!cases.is_empty(), "the model emitted no receipt traces");
    Ok(cases)
}

#[tokio::test]
async fn generated_receipt_traces_bind_the_native_signed_preparation() -> Result<()> {
    let cases = tokio::task::spawn_blocking(model_cases).await??;
    let mut facts = Facts::new()?;
    for case in cases {
        let mut records = Vec::new();
        let initial = case["initial"].as_array().context("initial records")?;
        let steps = case["trace"].as_array().context("receipt trace")?;
        let context = initial
            .first()
            .map(|record| &record["intent"])
            .or_else(|| steps.iter().find_map(|step| step["input"].get("intent")))
            .context("trace's authored input")?;
        for row in initial {
            let mut record = facts.proposal(&row["intent"], &row["prepared"]).await?;
            record.outcome = serde_json::from_value(row["outcome"].clone())?;
            record.acknowledged = row["acknowledged"].as_bool().context("acknowledgement")?;
            if row["retainedBytes"] == false {
                record.pending = None;
            }
            records.push(record);
        }
        for step in steps {
            assert_eq!(
                facts.records_json(&records)?,
                step["before"],
                "{} before",
                case["name"]
            );
            let input = &step["input"];
            let decision = match input["kind"].as_str().context("receipt action")? {
                "prepare" => {
                    let journal = facts.client(number(input, "journal")?)?;
                    let proposal = facts.proposal(&input["intent"], &input["proposal"]).await?;
                    let scope = records
                        .iter()
                        .filter(|record| record.intent.client_did == journal)
                        .cloned()
                        .collect::<Vec<_>>();
                    let (reserved, result) = prepare(
                        &journal,
                        &scope,
                        &proposal,
                        input["durable"]
                            .as_bool()
                            .context("durable acknowledgement")?,
                    );
                    if let Some(record) = reserved {
                        records.push(record);
                    }
                    facts.decision_json(result)?
                }
                "observe" | "acknowledge" => {
                    let journal = facts.client(number(input, "journal")?)?;
                    let caller = &input["caller"];
                    let client = facts.client(number(caller, "requester")?)?;
                    let request = identity(number(caller, "request")?);
                    let reference = facts.reference(&input["prepared"], context).await?;
                    if input["kind"] == "observe" {
                        observe(
                            &journal,
                            &mut records,
                            &client,
                            &request,
                            &reference,
                            serde_json::from_value(input["outcome"].clone())?,
                        );
                    } else {
                        acknowledge(&journal, &mut records, &client, &request, &reference);
                    }
                    Value::Null
                }
                "reconnect" => Value::Null,
                _ => anyhow::bail!("unknown modeled receipt action"),
            };
            assert_eq!(
                facts.records_json(&records)?,
                step["after"],
                "{} after {}",
                case["name"],
                input["kind"]
            );
            assert_eq!(decision, step["output"], "{} output", case["name"]);
        }
    }
    Ok(())
}

/// One desktop journal per modeled client.
#[derive(Default)]
struct Journals(BTreeMap<String, Arc<defra_node::Node>>);

impl Journals {
    async fn open(&mut self, client: &str) -> Result<&Arc<defra_node::Node>> {
        if !self.0.contains_key(client) {
            let node = crate::test_support::client_journal_node(client).await?;
            self.0.insert(client.to_owned(), node);
        }
        Ok(&self.0[client])
    }

    async fn shutdown(self) {
        for node in self.0.into_values() {
            node.shutdown().await.unwrap();
        }
    }
}

/// The modeled journal family as a set: grouped by caller key, then request.
/// Lean lists it in reservation order; journals are separate stores.
fn by_caller(rows: &Value) -> Result<Vec<Value>> {
    let mut rows = rows.as_array().context("modeled journal")?.clone();
    rows.sort_by_key(|row| {
        (
            row["intent"]["caller"]["requester"].as_u64(),
            row["intent"]["caller"]["request"].as_u64(),
        )
    });
    Ok(rows)
}

#[tokio::test]
async fn generated_receipt_traces_survive_durable_journal_roundtrips() -> Result<()> {
    use super::super::journal;
    use gents::store::graphql::graphql_with_transaction_retry;

    let cases = tokio::task::spawn_blocking(model_cases).await??;
    let result = async {
        let mut count = 0;
        for case in cases {
            let steps = case["trace"].as_array().context("receipt trace")?;
            if !case["initial"].as_array().context("initial")?.is_empty()
                || steps.iter().any(|step| step["input"]["durable"] == false)
            {
                continue;
            }
            // Each modeled journal is the local store of the client whose key
            // is its node identity, opened the way production opens it.
            let mut facts = Facts::new()?;
            let mut journals = Journals::default();
            let context = &steps[0]["input"]["intent"];
            for step in steps {
                let input = &step["input"];
                match input["kind"].as_str().context("receipt action")? {
                    "prepare" => {
                        let key = facts.client(number(input, "journal")?)?;
                        let proposal = facts.proposal(&input["intent"], &input["proposal"]).await?;
                        let output =
                            match journal::caller::reserve(journals.open(&key).await?, &proposal)
                                .await
                            {
                                Ok(record) => record.result(),
                                Err(_) if step["output"]["kind"] == "refused" => Decision::Refused,
                                Err(error) => {
                                    return Err(error.context(format!(
                                        "reserving modeled receipt in {}",
                                        case["name"]
                                    )))
                                }
                            };
                        assert_eq!(
                            facts.decision_json(output)?,
                            step["output"],
                            "{} reservation",
                            case["name"]
                        );
                    }
                    "observe" | "acknowledge" => {
                        let key = facts.client(number(input, "journal")?)?;
                        let mut binding = facts.intent(context)?;
                        binding.client_did =
                            facts.client(number(&input["caller"], "requester")?)?;
                        binding.request_id = identity(number(&input["caller"], "request")?);
                        let reference = facts.reference(&input["prepared"], context).await?;
                        let observation = if input["kind"] == "observe" {
                            Some(serde_json::from_value(input["outcome"].clone())?)
                        } else {
                            None
                        };
                        let _ = journal::caller::update(
                            journals.open(&key).await?,
                            &binding,
                            &reference,
                            observation,
                        )
                        .await;
                    }
                    "reconnect" => {}
                    _ => anyhow::bail!("unknown receipt action"),
                }
                let mut records = Vec::new();
                for (key, node) in &journals.0 {
                    let response = graphql_with_transaction_retry(
                        node,
                        "{ PendingChatSubmission { payload } }",
                        "test.receipt.snapshot",
                    )
                    .await?;
                    for row in response
                        .data
                        .as_ref()
                        .and_then(|data| data["PendingChatSubmission"].as_array())
                        .context("journal snapshot")?
                    {
                        let envelope: Value =
                            serde_json::from_str(row["payload"].as_str().context("payload")?)?;
                        let record = serde_json::from_value::<Record>(envelope["record"].clone())?;
                        assert_eq!(
                            &record.intent.client_did, key,
                            "{}: a journal holds only its own key's records",
                            case["name"]
                        );
                        records.push(record);
                    }
                }
                assert_eq!(
                    by_caller(&facts.records_json(&records)?)?,
                    by_caller(&step["after"])?,
                    "{} after {}",
                    case["name"],
                    input["kind"]
                );
                for record in &records {
                    let node = journals.open(&record.intent.client_did).await?;
                    let scoped = journal::caller::lookup(
                        node,
                        &record.intent.client_did,
                        &record.intent.node_did,
                        &record.intent.request_id,
                    )
                    .await?
                    .context("scoped durable caller binding")?;
                    assert_eq!(
                        facts.records_json(&[scoped])?,
                        facts.records_json(std::slice::from_ref(record))?
                    );
                    let recovered = journal::caller::recover(node, &record.intent)
                        .await?
                        .context("durable caller binding")?;
                    assert_eq!(recovered.reference, record.reference);
                    let pending =
                        journal::load(node, &record.intent.client_did, &record.intent.request_id)
                            .await?;
                    assert_eq!(pending.is_some(), record.outcome == Outcome::Unresolved);
                    if let Some(pending) = pending {
                        assert_eq!(
                            digest(pending.mutation.as_bytes()),
                            record.reference.signed_payload_hash
                        );
                    }
                    journal::forget(node, &record.intent.client_did, &record.intent.request_id)
                        .await?;
                    assert!(
                        journal::caller::recover(node, &record.intent)
                            .await?
                            .is_some(),
                        "cleanup erased caller correlation"
                    );
                }
            }
            journals.shutdown().await;
            count += 1;
        }
        anyhow::ensure!(count >= 18, "durable traces were not exercised");
        Ok::<(), anyhow::Error>(())
    }
    .await;
    result
}

#[tokio::test]
async fn caller_receipt_survives_competing_reservations_and_reopen() -> Result<()> {
    use super::super::journal;
    use std::sync::Arc;

    let cases = tokio::task::spawn_blocking(model_cases).await??;
    let case = cases
        .iter()
        .find(|case| case["name"] == "acknowledgement_loss")
        .context("modeled acknowledgement-loss trace")?;
    let trace = case["trace"].as_array().context("receipt trace")?;
    let first = &trace[0]["input"];
    let mut facts = Facts::new()?;
    let proposal = facts.proposal(&first["intent"], &first["proposal"]).await?;
    let home = tempfile::tempdir()?;
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(&proposal.intent.client_did)
            .data_path(home.path())
            .build()
            .await?,
    );
    node.add_collection(&crate::test_support::unbound_journal_schema())
        .await?;
    let (left, right) = tokio::join!(
        journal::caller::reserve(&node, &proposal),
        journal::caller::reserve(&node, &proposal),
    );
    let left = left?;
    let right = right?;
    assert_eq!(left.reference, right.reference);
    assert_eq!(facts.decision_json(left.result())?, trace[0]["output"]);
    node.shutdown().await.unwrap();
    drop(node);

    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(&proposal.intent.client_did)
            .data_path(home.path())
            .build()
            .await?,
    );
    let result = async {
        let recovered = journal::caller::recover(&node, &proposal.intent)
            .await?
            .context("reopened receipt")?;
        assert_eq!(facts.records_json(&[recovered])?, trace[0]["after"]);
        let accepted = journal::caller::update(
            &node,
            &proposal.intent,
            &proposal.reference,
            Some(serde_json::from_value(
                trace[1]["input"]["outcome"].clone(),
            )?),
        )
        .await?;
        assert_eq!(facts.records_json(&[accepted])?, trace[1]["after"]);
        let acknowledged =
            journal::caller::update(&node, &proposal.intent, &proposal.reference, None).await?;
        assert_eq!(facts.records_json(&[acknowledged])?, trace[2]["after"]);
        Ok::<(), anyhow::Error>(())
    }
    .await;
    node.shutdown().await.unwrap();
    drop(node);
    result?;

    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(&proposal.intent.client_did)
            .data_path(home.path())
            .build()
            .await?,
    );
    let result = async {
        let recovered = journal::caller::recover(&node, &proposal.intent)
            .await?
            .context("acknowledged receipt survived reopen")?;
        assert_eq!(facts.records_json(&[recovered])?, trace[2]["after"]);
        assert!(journal::load(
            &node,
            &proposal.intent.client_did,
            &proposal.intent.request_id
        )
        .await?
        .is_none());
        let retry = &trace.last().context("retry")?["input"];
        let replacement = facts.proposal(&retry["intent"], &retry["proposal"]).await?;
        let original = journal::caller::reserve(&node, &replacement).await?;
        assert_eq!(
            facts.decision_json(original.result())?,
            trace.last().unwrap()["output"]
        );
        Ok::<(), anyhow::Error>(())
    }
    .await;
    node.shutdown().await.unwrap();
    result
}

/// The owner effects run on a served runtime as its User: the modeled caller
/// is the User's client key and the modeled node its runtime. An accepted
/// outcome is a commit whose reply the caller lost; a confirmed rejection is a
/// request the owner refuses inside its transaction (its admission signature
/// does not verify). Replays and recovery after the outcome is retained reach
/// no owner: they run against an endpoint that records every request.
#[tokio::test]
async fn owner_outcomes_are_retained_before_the_caller_reply() -> Result<()> {
    use super::super::{delivery, journal, SubmissionNotDelivered};
    use crate::test_fixtures::ServedRuntimeHome;
    use wiremock::MockServer;

    let cases = tokio::task::spawn_blocking(model_cases).await??;
    for name in [
        "lost_accepted_reply",
        "confirmed_non_delivery_retains_identity",
        "confirmed_outcome_is_monotonic",
    ] {
        let stale = name == "confirmed_outcome_is_monotonic";
        let case = cases
            .iter()
            .find(|case| case["name"] == name)
            .context("modeled owner outcome")?;
        let trace = case["trace"].as_array().context("receipt trace")?;
        let first = &trace[0]["input"];
        let expected: Outcome = serde_json::from_value(trace[1]["input"]["outcome"].clone())?;
        let owner_outcome = if stale {
            serde_json::from_value(trace[2]["input"]["outcome"].clone())?
        } else {
            expected
        };
        let home = ServedRuntimeHome::start().await;
        let agent = home.provision_published_agent().await;
        let user = home.user().clone();
        let mut facts = Facts::new()?;
        facts.bind_client(number(&first["intent"]["caller"], "requester")?, user.did());
        facts.bind_node(number(&first["intent"], "node")?, home.node_did());
        let intent = facts.intent(&first["intent"])?;
        let prepared = &first["proposal"];
        let (result, mut create) = super::super::super::prepare::prepare_request_submission(
            &home.user_access(),
            &crate::client::ClientStore::default(),
            &identity(number(prepared, "session")?),
            home.node_did(),
            user.did(),
            user.as_ref(),
            AgentRequestAdmissionRecord::client(user.did()),
            format!("envelope-{}", number(&first["intent"], "envelope")?).as_str(),
            Some(&agent.agent_did),
            super::super::super::SubmitRequestOptions::default(),
            Some(&intent.request_id),
        )
        .await?;
        if owner_outcome == Outcome::NotDelivered {
            create.admission.signature[0] ^= 1;
        }
        let pending = UnresolvedSubmission::new(create, result)?;
        facts.bind_payload(&pending, number(prepared, "signedBytes")?);
        let proposal = Record::proposal(intent, pending);
        let node = crate::test_support::client_journal_node(user.did()).await?;
        let owner = home.user_access();
        let unreachable = MockServer::start().await;
        let silent = super::super::test_access(
            &format!("{}/api/v0/graphql", unreachable.uri()),
            user.as_ref(),
        );
        let result = async {
            let retained = journal::caller::reserve(&node, &proposal).await?;
            if stale {
                journal::caller::update(
                    &node,
                    &proposal.intent,
                    &proposal.reference,
                    Some(expected),
                )
                .await?;
            }
            if owner_outcome == Outcome::Accepted {
                gents::request::lifecycle::submit_client_request(
                    &owner,
                    &proposal.pending.as_ref().context("signed proposal")?.create,
                )
                .await?;
            }
            let reply = if expected == Outcome::Accepted && !stale {
                // The caller lost the owner's reply: its receipt read fails.
                let lost = delivery::reconcile(&node, Some(&silent), retained).await;
                assert!(matches!(
                    lost.unwrap_err(),
                    super::super::SubmissionError::Unknown(_)
                ));
                let unknown = journal::caller::recover(&node, &proposal.intent)
                    .await?
                    .context("unknown outcome remains retained")?;
                assert_eq!(
                    facts.records_json(std::slice::from_ref(&unknown))?,
                    trace[0]["after"]
                );
                delivery::reconcile(&node, Some(&owner), unknown).await
            } else {
                delivery::reconcile(&node, Some(&owner), retained).await
            };
            if expected == Outcome::Accepted {
                assert_eq!(reply?, proposal.reference.request);
            } else {
                let super::super::SubmissionError::NotDelivered(SubmissionNotDelivered::Rejected {
                    request,
                    ..
                }) = reply.expect_err("confirmed rejection")
                else {
                    panic!("rejection expected")
                };
                assert_eq!(request, proposal.reference.request);
            }
            let retained = journal::caller::recover(&node, &proposal.intent)
                .await?
                .context("outcome retained before reply")?;
            assert_eq!(
                facts.records_json(std::slice::from_ref(&retained))?,
                trace[1]["after"]
            );
            let before = unreachable.received_requests().await.unwrap().len();
            let replay = delivery::reconcile(&node, Some(&silent), retained).await;
            if expected == Outcome::Accepted {
                assert_eq!(replay?, proposal.reference.request);
            } else {
                assert!(matches!(
                    replay.unwrap_err(),
                    super::super::SubmissionError::NotDelivered(_)
                ));
            }
            let frozen = super::super::super::submit_request(
                &node,
                &crate::client::ClientStore::default(),
                &silent,
                "new-session-must-not-replace-the-original",
                &proposal.intent.node_did,
                "new-requester-must-not-replace-the-original",
                user.as_ref(),
                AgentRequestAdmissionRecord::client(user.did()),
                "",
                None,
                super::super::super::SubmitRequestOptions {
                    caller_intent: Some(proposal.intent.clone()),
                    ..Default::default()
                },
            )
            .await;
            if expected == Outcome::Accepted {
                assert_eq!(frozen?, proposal.reference.request);
            } else {
                assert!(matches!(
                    frozen.unwrap_err(),
                    super::super::SubmissionError::NotDelivered(_)
                ));
            }
            assert_eq!(
                unreachable.received_requests().await.unwrap().len(),
                before,
                "confirmed reply recovery and the frozen submit reach no owner"
            );
            let rows = owner
                .execute(&format!(
                    "{{AgentRequest(filter:{{request_id:{{_eq:\"{}\"}}}}){{admission_signature}}}}",
                    proposal.intent.request_id
                ))
                .await?;
            assert_eq!(
                rows["data"]["AgentRequest"]
                    .as_array()
                    .context("owner rows")?
                    .len(),
                usize::from(owner_outcome == Outcome::Accepted),
                "the owner holds the original request exactly when it accepted it"
            );
            Ok::<(), anyhow::Error>(())
        }
        .await;
        node.shutdown().await.unwrap();
        home.shutdown().await;
        result.with_context(|| format!("modeled owner outcome {name}"))?;
    }
    Ok(())
}

async fn check_recovery_route(activate_route: bool) -> Result<()> {
    use super::super::{journal, SubmissionError};
    use crate::client::{ClientCore, ClientCoreOptions, DesktopPaths};
    use gents::store::config_client::ConfigAccess;
    use std::sync::Arc;

    let cases = tokio::task::spawn_blocking(model_cases).await??;
    let case = cases
        .iter()
        .find(|case| case["name"] == "lost_accepted_reply")
        .context("recovery model")?;
    let trace = case["trace"].as_array().context("receipt trace")?;
    let mut facts = Facts::new()?;
    let mut proposal = facts
        .proposal(&trace[0]["input"]["intent"], &trace[0]["input"]["proposal"])
        .await?;
    let home = tempfile::tempdir()?;
    let core = Arc::new(
        ClientCore::start_with_paths_and_options(
            DesktopPaths::from_root(home.path()),
            ClientCoreOptions::local_simulated_route(),
        )
        .await?,
    );
    proposal.intent.client_did = core.node_identity().did().to_owned();
    let node = core.node_arc();
    let result = async {
        journal::caller::reserve(&node, &proposal).await?;
        let unresolved = core
            .recover_caller_submission(&proposal.intent)
            .await
            .expect_err("missing runtime authority must remain unconfirmed");
        let SubmissionError::Unknown(unknown) = unresolved else {
            anyhow::bail!("unknown outcome expected")
        };
        let pending = unknown.pending;
        assert_eq!(
            digest(pending.mutation.as_bytes()),
            proposal.reference.signed_payload_hash
        );
        let rows = ConfigAccess::local(Arc::clone(&node))
            .execute("{ AgentRequest { request_id } }")
            .await?;
        assert_eq!(
            rows["data"]["AgentRequest"],
            json!([]),
            "missing remote route must not write into the client replica"
        );
        if activate_route {
            core.add_local_standard_peer_for_test(&proposal.intent.node_did)
                .await?;
        } else {
            journal::caller::update(
                &node,
                &proposal.intent,
                &proposal.reference,
                Some(serde_json::from_value(
                    trace[1]["input"]["outcome"].clone(),
                )?),
            )
            .await?;
        }
        core.store()
            .set_focused_request_id(Some("reader-current-turn".into()));
        let accepted = core
            .recover_caller_submission(&proposal.intent)
            .await?
            .context("confirmed receipt without runtime")?;
        assert_eq!(accepted, proposal.reference.request);
        assert_eq!(
            core.store().focused_request_id().as_deref(),
            Some("reader-current-turn"),
            "a late recovery must preserve the selected request"
        );
        let retained = journal::caller::recover(&node, &proposal.intent)
            .await?
            .context("confirmed binding")?;
        assert_eq!(
            retained.outcome,
            serde_json::from_value::<Outcome>(trace[1]["input"]["outcome"].clone())?
        );
        if activate_route {
            let rows = ConfigAccess::local(Arc::clone(&node))
                .execute("{ AgentRequest { request_id } }")
                .await?;
            assert_eq!(
                rows["data"]["AgentRequest"],
                json!([{"request_id":proposal.reference.request.request_id}])
            );
        }

        let frozen = core
            .submit_request_with_options(
                "new-session",
                &proposal.intent.node_did,
                "",
                None,
                super::super::super::SubmitRequestOptions {
                    caller_intent: Some(proposal.intent.clone()),
                    ..Default::default()
                },
            )
            .await?;
        assert_eq!(frozen, proposal.reference.request);
        let reference = core
            .caller_submission_reference(&proposal.intent)
            .await?
            .context("caller receipt reference")?;
        core.acknowledge_caller_submission(&proposal.intent, &reference)
            .await?;
        assert_eq!(
            core.recover_caller_submission(&proposal.intent).await?,
            Some(accepted)
        );
        let foreign = Intent::new(
            "different-client".into(),
            proposal.intent.node_did.clone(),
            proposal.intent.request_id.clone(),
            b"same operation",
        );
        assert!(core.recover_caller_submission(&foreign).await.is_err());
        assert_eq!(
            core.store().focused_request_id().as_deref(),
            Some("reader-current-turn")
        );
        Ok::<(), anyhow::Error>(())
    }
    .await;
    core.shutdown().await?;
    result
}

#[tokio::test]
async fn recovery_without_a_runtime_route_preserves_the_original_preparation() -> Result<()> {
    check_recovery_route(false).await
}

#[tokio::test]
async fn recovery_preserves_the_existing_ready_embedded_delivery_route() -> Result<()> {
    check_recovery_route(true).await
}

#[tokio::test]
async fn first_submission_uses_the_caller_identity_and_freezes_goal_assignment() -> Result<()> {
    use super::super::journal;
    use crate::client::{ClientStore, SubmitRequestOptions};
    use gents::store::config_client::ConfigAccess;
    use gents_protocol::request_input::{GoalAssignmentInput, RequestInput};
    use std::sync::Arc;

    let cases = tokio::task::spawn_blocking(model_cases).await??;
    let case = cases
        .iter()
        .find(|case| case["name"] == "lost_accepted_reply")
        .context("submission model")?;
    let trace = case["trace"].as_array().context("receipt trace")?;
    let first = &trace[0]["input"];
    let mut facts = Facts::new()?;
    let mut intent = facts.intent(&first["intent"])?;
    let session = identity(number(&first["proposal"], "session")?);
    let input = RequestInput {
        goal_assignment: Some(GoalAssignmentInput {
            goal_id: "historical-goal".into(),
            root_request_doc_id: None,
        }),
        ..Default::default()
    };
    let command = serde_json::to_vec(
        &json!({"nodeDid":intent.node_did, "requestId":intent.request_id,
        "sessionId":session, "agentDid":"fixture-agent", "content":"authored submission", "input":input}),
    )?;
    intent = Intent::new(
        intent.node_did.clone(),
        intent.node_did,
        intent.request_id,
        &command,
    );
    facts.clients.insert(
        intent.client_did.clone(),
        number(&first["intent"]["caller"], "requester")?,
    );
    facts.envelopes.insert(
        intent.envelope_hash.clone(),
        number(&first["intent"], "envelope")?,
    );
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(&intent.client_did)
            .build()
            .await?,
    );
    gents::ensure_runtime_schemas(&node).await?;
    let access = ConfigAccess::local(Arc::clone(&node));
    let result = async {
        let first_result = super::super::super::submit_request(
            &node,
            &ClientStore::default(),
            &access,
            &session,
            &intent.node_did,
            &intent.node_did,
            &facts.nodes[&number(&first["intent"], "node")?],
            AgentRequestAdmissionRecord::local_self(&intent.node_did),
            "authored submission",
            Some("fixture-agent"),
            SubmitRequestOptions {
                caller_intent: Some(intent.clone()),
                input: input.clone(),
                ..Default::default()
            },
        )
        .await?;
        assert_eq!(first_result.request_id, intent.request_id);
        let record = journal::caller::recover(&node, &intent)
            .await?
            .context("first submission retained")?;
        facts.payloads.insert(
            record.reference.signed_payload_hash.clone(),
            number(&first["proposal"], "signedBytes")?,
        );
        assert_eq!(
            facts.records_json(std::slice::from_ref(&record))?,
            trace[1]["after"]
        );
        assert_eq!(
            record
                .pending
                .as_ref()
                .context("signed preparation")?
                .create
                .input
                .goal_assignment,
            input.goal_assignment
        );
        let recovered = super::super::super::submit_request(
            &node,
            &ClientStore::default(),
            &access,
            "different-derived-session",
            &intent.node_did,
            &intent.node_did,
            &facts.nodes[&number(&first["intent"], "node")?],
            AgentRequestAdmissionRecord::local_self(&intent.node_did),
            "authored submission",
            None,
            SubmitRequestOptions {
                caller_intent: Some(intent.clone()),
                ..Default::default()
            },
        )
        .await?;
        assert_eq!(recovered, first_result);
        let rows = access.execute("{ AgentRequest { request_id } }").await?;
        assert_eq!(
            rows["data"]["AgentRequest"],
            json!([{"request_id":intent.request_id}])
        );
        let unchanged = journal::caller::recover(&node, &intent)
            .await?
            .context("recovered preparation")?;
        assert_eq!(unchanged.reference, record.reference);
        assert_eq!(
            unchanged.pending.as_ref().unwrap().mutation,
            record.pending.as_ref().unwrap().mutation
        );
        Ok::<(), anyhow::Error>(())
    }
    .await;
    node.shutdown().await.unwrap();
    result
}
