use super::*;
use anyhow::{Context, Result};
use gents::store::identity::{KeyIdentity, NodeIdentity};
use gents_protocol::goal_control::{
    GoalControlOutcome, GoalOperation, GoalRevision, SESSION_GOAL_CONTROL_VERSION,
};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;

struct Facts {
    root: tempfile::TempDir,
    nodes: BTreeMap<u64, KeyIdentity>,
    digests: BTreeMap<String, u64>,
    authored: BTreeMap<String, u64>,
    commands: BTreeMap<String, SessionGoalControl>,
}

fn number(value: &Value, name: &str) -> Result<u64> {
    value[name]
        .as_u64()
        .with_context(|| format!("modeled {name}"))
}

fn suffix(value: &str) -> Result<u64> {
    value
        .rsplit('-')
        .next()
        .context("fixture identity")?
        .parse()
        .map_err(Into::into)
}

impl Facts {
    fn new() -> Result<Self> {
        Ok(Self {
            root: tempfile::tempdir()?,
            nodes: BTreeMap::new(),
            digests: BTreeMap::new(),
            authored: BTreeMap::new(),
            commands: BTreeMap::new(),
        })
    }

    fn node(&mut self, id: u64) -> Result<String> {
        if !self.nodes.contains_key(&id) {
            self.nodes.insert(
                id,
                KeyIdentity::load_or_create(self.root.path().join(format!("node-{id}.key")), None)?,
            );
        }
        Ok(self.nodes[&id].did().into())
    }

    fn intent(&mut self, value: &Value) -> Result<Intent> {
        let authored = number(value, "authored")?;
        let intent = Intent::new(
            format!("client-{}", number(value, "client")?),
            self.node(number(value, "node")?)?,
            format!("command-{}", number(value, "command")?),
            format!("session-{}", number(value, "session")?),
            format!("authored-{authored}").as_bytes(),
        );
        self.authored.insert(intent.authored_hash.clone(), authored);
        Ok(intent)
    }

    async fn command(&mut self, value: &Value, context: &Value) -> Result<SessionGoalControl> {
        let node_id = number(context, "node")?;
        let did = self.node(node_id)?;
        let digest = number(value, "digest")?;
        let at = chrono::DateTime::parse_from_rfc3339("2026-10-26T00:00:00Z")?
            + chrono::Duration::seconds(i64::try_from(digest)?);
        let created = at.to_rfc3339();
        let session = format!("session-{}", number(context, "session")?);
        let initial_request = if let Some(id) = value["initialRequest"].as_u64() {
            let mut request = AgentRequestCreate::base(
                RequestPurpose::Normal,
                format!("request-{id}"),
                &did,
                &did,
                "fixture-agent",
                &session,
                "goal objective",
                "interactive",
                &created,
                AgentRequestAdmissionRecord::local_self(&did),
            );
            request.input.captured_context = vec![serde_json::from_value(json!({
                "kind":"file", "source":"editor", "capturedAt":created,
                "uri":"file:///workspace/retained.rs", "terminalName":null,
                "workspaceUri":"file:///workspace", "version":7, "unsaved":true,
                "range":null, "content":"captured before the lost reply\n"
            }))?];
            request.input.attachments = vec![serde_json::from_value(json!({
                "name": "capture.png", "mediaType": "image/png", "data": "aW1hZ2U="
            }))?];
            request.admission.signature = self.nodes[&node_id]
                .sign(&request.signing_payload())
                .await?;
            Some(Box::new(request))
        } else {
            None
        };
        let command = SessionGoalControl {
            version: SESSION_GOAL_CONTROL_VERSION,
            command_id: format!("command-{}", number(value, "command")?),
            requester_did: did.clone(),
            requester_peer_id: None,
            node_did: did,
            session_id: session,
            session_doc_id: Some("fixture-session-document".into()),
            issued_at: created,
            expires_at: (at + chrono::Duration::minutes(5)).to_rfc3339(),
            expected: initial_request.is_none().then(|| GoalRevision {
                document: "fixture-goal".into(),
                composite: "fixture-revision".into(),
            }),
            operation: if initial_request.is_some() {
                GoalOperation::Create {
                    objective: "goal objective".into(),
                    token_budget: None,
                }
            } else {
                GoalOperation::Pause {}
            },
            initial_request,
            signature: Vec::new(),
        };
        let command =
            gents::goal::prepare_session_goal_control(&self.nodes[&node_id], command).await?;
        let hash = command.computed_digest()?;
        self.digests.insert(hash.clone(), digest);
        self.commands.insert(hash, command.clone());
        Ok(command)
    }

    async fn receipt(
        &mut self,
        value: &Value,
        context: &Value,
    ) -> Result<SessionGoalControlReceipt> {
        let abstract_digest = number(value, "digest")?;
        let hash = self
            .digests
            .iter()
            .find(|(_, n)| **n == abstract_digest)
            .map(|(hash, _)| hash.clone())
            .unwrap_or_else(|| {
                format!(
                    "sha256:{:x}",
                    Sha256::digest(format!("foreign-{abstract_digest}").as_bytes())
                )
            });
        self.digests.insert(hash.clone(), abstract_digest);
        let id = number(context, "node")?;
        let did = self.node(id)?;
        let mut receipt = SessionGoalControlReceipt {
            version: SESSION_GOAL_CONTROL_VERSION,
            command_id: format!("command-{}", number(value, "command")?),
            command_digest: hash.clone(),
            requester_did: did.clone(),
            node_did: did,
            session_id: format!("session-{}", number(context, "session")?),
            outcome: if value["outcome"] == "applied" {
                GoalControlOutcome::Applied {
                    goal_id: "fixture-goal".into(),
                    request_id: self
                        .commands
                        .get(&hash)
                        .and_then(|c| c.initial_request.as_ref().map(|r| r.request_id.clone())),
                }
            } else {
                GoalControlOutcome::Rejected {
                    reason: "modeled rejection".into(),
                }
            },
            processed_at: "2026-10-26T00:01:00Z".into(),
            signature: Vec::new(),
        };
        receipt.signature = self.nodes[&id].sign(&receipt.signing_payload()?).await?;
        Ok(receipt)
    }

    async fn record(&mut self, value: &Value) -> Result<Record> {
        let intent = self.intent(&value["intent"])?;
        let command = self.command(&value["prepared"], &value["intent"]).await?;
        let reference = Reference {
            command_id: command.command_id.clone(),
            command_digest: command.computed_digest()?,
            initial_request_id: command
                .initial_request
                .as_ref()
                .map(|r| r.request_id.clone()),
            requester_did: command.requester_did.clone(),
        };
        let receipt = if value["receipt"].is_null() {
            None
        } else {
            Some(self.receipt(&value["receipt"], &value["intent"]).await?)
        };
        Ok(Record {
            intent,
            reference,
            prepared: (value["retainedBytes"] != false).then_some(command),
            receipt,
            acknowledged: value["acknowledged"] == true,
        })
    }

    fn reference_json(&self, reference: &Reference) -> Result<Value> {
        Ok(
            json!({"command":suffix(&reference.command_id)?, "digest":self.digests[&reference.command_digest],
            "initialRequest":reference.initial_request_id.as_deref().map(suffix).transpose()?}),
        )
    }

    fn receipt_json(&self, receipt: &SessionGoalControlReceipt) -> Result<Value> {
        Ok(
            json!({"command":suffix(&receipt.command_id)?, "digest":self.digests[&receipt.command_digest],
            "outcome":if matches!(receipt.outcome,GoalControlOutcome::Applied {..}) {"applied"} else {"rejected"}}),
        )
    }

    fn records_json(&self, records: &[Record]) -> Result<Value> {
        records.iter().map(|record| {
            let node = self.nodes.iter().find(|(_,key)|key.did()==record.intent.node_did).context("native fixture identity")?.0;
            Ok(json!({"intent":{"client":suffix(&record.intent.client_did)?,"node":node,
                "command":suffix(&record.intent.command_id)?,"session":suffix(&record.intent.session_id)?,
                "authored":self.authored[&record.intent.authored_hash]},
                "prepared":self.reference_json(&record.reference)?,
                "receipt":record.receipt.as_ref().map(|r|self.receipt_json(r)).transpose()?,
                "retainedBytes":record.prepared.is_some(),"acknowledged":record.acknowledged}))
        }).collect::<Result<Vec<_>>>().map(Value::from)
    }

    fn output(&self, decision: Decision, publication: Option<Reference>) -> Result<Value> {
        let result = match decision {
            Decision::Refused => json!({"kind":"refused"}),
            Decision::Unconfirmed => json!({"kind":"unconfirmed"}),
            Decision::Pending(reference) => {
                json!({"kind":"pending","prepared":self.reference_json(&reference)?})
            }
            Decision::Confirmed(reference, receipt) => {
                json!({"kind":"confirmed","prepared":self.reference_json(&reference)?,"receipt":self.receipt_json(&receipt)?})
            }
        };
        Ok(
            json!({"result":result,"publish":publication.as_ref().map(|r|self.reference_json(r)).transpose()?}),
        )
    }
}

fn model_cases() -> Result<Vec<Value>> {
    let value: Value = gents_lean_contract::load_contract_snapshot()?;
    Ok(value["goal_client_delivery_cases"]
        .as_array()
        .context("executed Goal delivery traces")?
        .clone())
}

#[tokio::test]
async fn generated_goal_delivery_histories_bind_native_signed_preparations() -> Result<()> {
    let cases = tokio::task::spawn_blocking(model_cases).await??;
    assert_eq!(cases.len(), 21);
    for case in cases {
        let mut facts = Facts::new()?;
        let mut records = Vec::new();
        for record in case["initial"].as_array().context("initial journal")? {
            records.push(facts.record(record).await?);
        }
        for step in case["trace"].as_array().context("modeled trace")? {
            assert_eq!(
                facts.records_json(&records)?,
                step["before"],
                "{} before",
                case["name"]
            );
            let action = &step["input"];
            let output = match action["kind"].as_str().context("modeled action")? {
                "reserve" => {
                    let proposal = facts.record(&json!({"intent":action["intent"],"prepared":action["prepared"],"receipt":null,"retainedBytes":true,"acknowledged":false})).await?;
                    let (staged, decision) =
                        reserve(&records, &proposal, action["durable"] == true);
                    if let Some(record) = staged {
                        records.push(record);
                    }
                    facts.output(decision, None)?
                }
                "query" => {
                    let intent = facts.intent(&action["intent"])?;
                    facts.output(query(&records, &intent), None)?
                }
                "resume" => {
                    let intent = facts.intent(&action["intent"])?;
                    let (decision, publication) = resume(&records, &intent);
                    facts.output(decision, publication)?
                }
                "observe" => {
                    let intent = facts.intent(&action["intent"])?;
                    let receipt = facts.receipt(&action["receipt"], &action["intent"]).await?;
                    observe(&mut records, &intent, &receipt, action["verified"] == true);
                    Value::Null
                }
                "acknowledge" => {
                    let intent = facts.intent(&action["intent"])?;
                    let command = facts
                        .command(&action["prepared"], &action["intent"])
                        .await?;
                    let reference = Reference {
                        command_id: command.command_id.clone(),
                        command_digest: command.computed_digest()?,
                        initial_request_id: command
                            .initial_request
                            .as_ref()
                            .map(|r| r.request_id.clone()),
                        requester_did: command.requester_did,
                    };
                    acknowledge(&mut records, &intent, &reference);
                    Value::Null
                }
                "reconnect" => {
                    records = serde_json::from_slice(&serde_json::to_vec(&records)?)?;
                    Value::Null
                }
                other => anyhow::bail!("unhandled modeled action {other}"),
            };
            assert_eq!(
                output, step["output"],
                "{} {} output",
                case["name"], action["kind"]
            );
            assert_eq!(
                facts.records_json(&records)?,
                step["after"],
                "{} {} journal",
                case["name"],
                action["kind"]
            );
        }
    }
    Ok(())
}

#[path = "tests/durable.rs"]
mod durable;
