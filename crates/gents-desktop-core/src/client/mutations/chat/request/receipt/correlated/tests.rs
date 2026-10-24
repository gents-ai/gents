use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};
use gents::identity::{KeyIdentity, NodeIdentity};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use serde_json::{json, Value};
use uuid::Uuid;

use super::*;

struct Facts {
    root: tempfile::TempDir,
    nodes: BTreeMap<u64, KeyIdentity>,
    envelopes: BTreeMap<String, u64>,
    payloads: BTreeMap<String, u64>,
}

impl Facts {
    fn new() -> Result<Self> {
        Ok(Self {
            root: tempfile::tempdir()?,
            nodes: BTreeMap::new(),
            envelopes: BTreeMap::new(),
            payloads: BTreeMap::new(),
        })
    }

    fn node(&mut self, value: u64) -> Result<String> {
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

    fn intent(&mut self, input: &Value) -> Result<Intent> {
        let envelope = number(input, "envelope")?;
        let intent = Intent::new(
            format!("client-{}", number(&input["caller"], "requester")?),
            self.node(number(input, "node")?)?,
            identity(number(&input["caller"], "request")?),
            format!("authored-envelope-{envelope}").as_bytes(),
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
                agent_id: Some("fixture-agent".into()),
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
                .map(|(id, _)| *id).context("known native node")?;
            Ok(json!({
                "intent": {
                    "caller": { "requester": record.intent.client_did.strip_prefix("client-").context("caller")?.parse::<u64>()?,
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
#[tokio::test]
async fn generated_receipt_traces_bind_the_native_signed_preparation() -> Result<()> {
    let proofs = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../gents/proofs");
    let output = Command::new("lake")
        .args([
            "env",
            "lean",
            "--run",
            "Proofs/Conformance/ClientSubmissionReceipt.lean",
        ])
        .current_dir(proofs)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "executing receipt owner: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cases: Vec<Value> = serde_json::from_slice(&output.stdout)?;
    anyhow::ensure!(!cases.is_empty(), "the model emitted no receipt traces");
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
                    let proposal = facts.proposal(&input["intent"], &input["proposal"]).await?;
                    let (reserved, result) = prepare(
                        &records,
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
                    let caller = &input["caller"];
                    let client = format!("client-{}", number(caller, "requester")?);
                    let request = identity(number(caller, "request")?);
                    let reference = facts.reference(&input["prepared"], context).await?;
                    if input["kind"] == "observe" {
                        observe(
                            &mut records,
                            &client,
                            &request,
                            &reference,
                            serde_json::from_value(input["outcome"].clone())?,
                        );
                    } else {
                        acknowledge(&mut records, &client, &request, &reference);
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
