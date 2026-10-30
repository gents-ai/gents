//! Journaled simulate-before-sign submission: signed bytes are persisted before
//! broadcast so recovery rebroadcasts the same transaction hash, never a new nonce.

use rapidhash::RapidHashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use alloy_primitives::{Address, U256};
use anyhow::{anyhow, bail, Context, Result};
use defra_node::EmbeddedNode;
use serde::Deserialize;
use serde_json::{json, Value};
use sha3::{Digest, Keccak256};

use super::keys::address_from_secret;
use super::rpc::{parse_hex_u64, EthRpcClient, JsonRpcTransport};

mod encoding;
mod journal;
mod prepare;

use encoding::{hex_encode, raw_transaction_hash, sign_eip1559};
use journal::{create_submission, load_submission, update_submission};
use prepare::{gas_fees, next_nonce, simulate};

const STATUS_PREPARED: &str = "prepared";
const STATUS_SUBMITTED_UNKNOWN: &str = "submitted_unknown";
const STATUS_CONFIRMED_SUCCESS: &str = "confirmed_success";
const STATUS_CONFIRMED_REVERTED: &str = "confirmed_reverted";

#[derive(Debug, Clone, Copy)]
enum SubmissionEvent {
    Broadcast,
    ObserveSuccess,
    ObserveRevert,
}

fn next_submission_status(current: &str, event: SubmissionEvent) -> Option<&'static str> {
    match (current, event) {
        (STATUS_PREPARED | STATUS_SUBMITTED_UNKNOWN, SubmissionEvent::Broadcast) => {
            Some(STATUS_SUBMITTED_UNKNOWN)
        }
        (STATUS_PREPARED | STATUS_SUBMITTED_UNKNOWN, SubmissionEvent::ObserveSuccess) => {
            Some(STATUS_CONFIRMED_SUCCESS)
        }
        (STATUS_PREPARED | STATUS_SUBMITTED_UNKNOWN, SubmissionEvent::ObserveRevert) => {
            Some(STATUS_CONFIRMED_REVERTED)
        }
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct GasCaps {
    pub(crate) max_gas: Option<u64>,
    pub(crate) max_fee_per_gas: Option<u128>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubmitRequest {
    pub(crate) principal_did: String,
    pub(crate) chain_id: u64,
    pub(crate) from: String,
    pub(crate) to: Option<String>,
    pub(crate) value: U256,
    pub(crate) data: Vec<u8>,
    pub(crate) caps: GasCaps,
    pub(crate) idempotency_key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubmitStatus {
    ConfirmedSuccess,
    ConfirmedReverted,
    SubmittedUnknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubmitReceipt {
    pub(crate) tx_hash: String,
    pub(crate) status: SubmitStatus,
    pub(crate) receipt: Option<Value>,
}

type NonceSlot = Arc<tokio::sync::Mutex<()>>;

#[derive(Debug, Default)]
pub(crate) struct NonceGate {
    inner: Mutex<RapidHashMap<(String, u64), NonceSlot>>,
}

impl NonceGate {
    fn slot(&self, address: &str, chain_id: u64) -> Arc<tokio::sync::Mutex<()>> {
        let key = (address.to_ascii_lowercase(), chain_id);
        self.inner
            .lock()
            .expect("eth nonce gate poisoned")
            .entry(key)
            .or_default()
            .clone()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SubmitOptions {
    pub(crate) receipt_attempts: u32,
    pub(crate) receipt_interval: Duration,
}

pub(crate) fn global_nonce_gate() -> &'static NonceGate {
    static GATE: OnceLock<NonceGate> = OnceLock::new();
    GATE.get_or_init(NonceGate::default)
}

impl Default for SubmitOptions {
    fn default() -> Self {
        Self {
            receipt_attempts: 8,
            receipt_interval: Duration::from_millis(500),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct SubmissionRecord {
    #[serde(rename = "_docID")]
    doc_id: String,
    submission_key: String,
    principal_did: String,
    chain_id: i64,
    request_hash: String,
    from_address: String,
    nonce: i64,
    raw_transaction: String,
    tx_hash: String,
    status: String,
    receipt_json: Option<String>,
}

pub(crate) async fn submit_transaction<T>(
    node: &Arc<EmbeddedNode>,
    client: &EthRpcClient<T>,
    secret: &[u8; 32],
    request: SubmitRequest,
    nonces: &NonceGate,
    options: SubmitOptions,
) -> Result<SubmitReceipt>
where
    T: JsonRpcTransport,
{
    validate_request(&request)?;
    let derived = address_from_secret(secret).context("deriving submit signer address")?;
    if !derived.eq_ignore_ascii_case(&request.from) {
        bail!(
            "signing key address {derived} does not match from address {}",
            request.from
        );
    }
    let submission_key = submission_key(&request);
    let request_hash = request_hash(&request)?;
    let nonce_slot = nonces.slot(&request.from, request.chain_id);
    let _nonce_lock = nonce_slot.lock().await;
    if let Some(existing) = load_submission(node, &submission_key).await? {
        verify_existing(&existing, &request, &request_hash)?;
        return resume_submission(node, client, existing, options).await;
    }

    let nonce = next_nonce(node, client, &request.from, request.chain_id).await?;
    let (gas_limit, max_fee_per_gas, max_priority_fee_per_gas) = gas_fees(client, &request).await?;
    simulate(
        client,
        &request,
        nonce,
        gas_limit,
        max_fee_per_gas,
        max_priority_fee_per_gas,
    )
    .await?;
    let raw_transaction = sign_eip1559(
        secret,
        request.chain_id,
        nonce,
        gas_limit,
        max_fee_per_gas,
        max_priority_fee_per_gas,
        request.to.as_deref(),
        request.value,
        &request.data,
    )?;
    let tx_hash = raw_transaction_hash(&raw_transaction)?;
    let prepared = SubmissionRecord {
        doc_id: String::new(),
        submission_key: submission_key.clone(),
        principal_did: request.principal_did.trim().to_string(),
        chain_id: i64::try_from(request.chain_id).context("chain_id exceeds DefraDB Int")?,
        request_hash,
        from_address: request.from.to_ascii_lowercase(),
        nonce: i64::try_from(nonce).context("nonce exceeds DefraDB Int")?,
        raw_transaction,
        tx_hash,
        status: STATUS_PREPARED.to_string(),
        receipt_json: None,
    };

    if let Err(create_error) = create_submission(node, &prepared).await {
        if let Some(existing) = load_submission(node, &submission_key).await? {
            verify_existing(&existing, &request, &prepared.request_hash)?;
            return resume_submission(node, client, existing, options).await;
        }
        return Err(create_error).context("persisting prepared Ethereum submission");
    }
    let persisted = load_submission(node, &submission_key)
        .await?
        .ok_or_else(|| anyhow!("prepared Ethereum submission was not readable after create"))?;
    resume_submission(node, client, persisted, options).await
}

fn validate_request(request: &SubmitRequest) -> Result<()> {
    if request.idempotency_key.trim().is_empty() {
        bail!("eth submit requires an idempotency key");
    }
    if request.principal_did.trim().is_empty() {
        bail!("eth submit requires principal_did");
    }
    if request.chain_id == 0 {
        bail!("eth submit chain_id must be positive");
    }
    let from: Address = request
        .from
        .parse()
        .context("invalid submit from address")?;
    if from == Address::ZERO {
        bail!("eth submit from address must not be zero");
    }
    if let Some(to) = request.to.as_deref() {
        let _: Address = to.parse().context("invalid submit to address")?;
    }
    Ok(())
}

fn submission_key(request: &SubmitRequest) -> String {
    format!(
        "{}:{}:{}",
        request.principal_did.trim(),
        request.chain_id,
        request.idempotency_key.trim()
    )
}

fn request_hash(request: &SubmitRequest) -> Result<String> {
    let canonical = serde_json::to_vec(&json!({
        "principal_did": request.principal_did.trim(),
        "chain_id": request.chain_id,
        "from": request.from.to_ascii_lowercase(),
        "to": request.to.as_deref().map(str::to_ascii_lowercase),
        "value": request.value.to_string(),
        "data": format!("0x{}", hex_encode(&request.data)),
        "max_gas": request.caps.max_gas,
        "max_fee_per_gas": request.caps.max_fee_per_gas.map(|value| value.to_string()),
    }))?;
    Ok(format!("0x{}", hex_encode(&Keccak256::digest(canonical))))
}

fn verify_existing(
    existing: &SubmissionRecord,
    request: &SubmitRequest,
    expected_request_hash: &str,
) -> Result<()> {
    if existing.request_hash != expected_request_hash {
        bail!(
            "idempotency key {:?} was already used for a different Ethereum request",
            request.idempotency_key
        );
    }
    let request_chain_id =
        i64::try_from(request.chain_id).context("chain_id exceeds DefraDB Int")?;
    if existing.principal_did != request.principal_did.trim()
        || existing.chain_id != request_chain_id
        || !existing.from_address.eq_ignore_ascii_case(&request.from)
    {
        bail!("persisted Ethereum submission identity does not match the request");
    }
    let expected_hash = raw_transaction_hash(&existing.raw_transaction)?;
    if !expected_hash.eq_ignore_ascii_case(&existing.tx_hash) {
        bail!("persisted Ethereum submission raw transaction hash is inconsistent");
    }
    if existing.nonce < 0 {
        bail!("persisted Ethereum submission has a negative nonce");
    }
    Ok(())
}

async fn resume_submission<T>(
    node: &Arc<EmbeddedNode>,
    client: &EthRpcClient<T>,
    record: SubmissionRecord,
    options: SubmitOptions,
) -> Result<SubmitReceipt>
where
    T: JsonRpcTransport,
{
    if let Some(receipt) = terminal_receipt(&record)? {
        return Ok(receipt);
    }

    if let Some(receipt) = fetch_receipt(client, &record.tx_hash).await? {
        return persist_receipt(node, &record, receipt).await;
    }

    let send_result = client.send_raw_transaction(&record.raw_transaction).await;
    if let Ok(value) = &send_result {
        let returned = value
            .as_str()
            .ok_or_else(|| anyhow!("eth_sendRawTransaction did not return a hash: {value}"))?;
        if !returned.eq_ignore_ascii_case(&record.tx_hash) {
            bail!(
                "node returned transaction hash {returned}, expected {}",
                record.tx_hash
            );
        }
    }
    let submitted_status = next_submission_status(&record.status, SubmissionEvent::Broadcast)
        .ok_or_else(|| {
            anyhow!(
                "cannot broadcast Ethereum submission in state {}",
                record.status
            )
        })?;
    update_submission(node, &record.doc_id, submitted_status, None).await?;

    for attempt in 0..options.receipt_attempts.max(1) {
        if attempt > 0 && !options.receipt_interval.is_zero() {
            tokio::time::sleep(options.receipt_interval).await;
        }
        if let Some(receipt) = fetch_receipt(client, &record.tx_hash).await? {
            return persist_receipt(node, &record, receipt).await;
        }
    }

    Ok(SubmitReceipt {
        tx_hash: record.tx_hash,
        status: SubmitStatus::SubmittedUnknown,
        receipt: None,
    })
}

fn terminal_receipt(record: &SubmissionRecord) -> Result<Option<SubmitReceipt>> {
    let status = match record.status.as_str() {
        STATUS_CONFIRMED_SUCCESS => Some(SubmitStatus::ConfirmedSuccess),
        STATUS_CONFIRMED_REVERTED => Some(SubmitStatus::ConfirmedReverted),
        _ => None,
    };
    let Some(status) = status else {
        return Ok(None);
    };
    let receipt = record
        .receipt_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .context("decoding persisted Ethereum receipt")?;
    Ok(Some(SubmitReceipt {
        tx_hash: record.tx_hash.clone(),
        status,
        receipt,
    }))
}

async fn fetch_receipt<T>(client: &EthRpcClient<T>, tx_hash: &str) -> Result<Option<Value>>
where
    T: JsonRpcTransport,
{
    let value = client
        .transaction_receipt(tx_hash)
        .await
        .context("eth_getTransactionReceipt")?;
    Ok((!value.is_null()).then_some(value))
}

async fn persist_receipt(
    node: &Arc<EmbeddedNode>,
    record: &SubmissionRecord,
    receipt: Value,
) -> Result<SubmitReceipt> {
    let success = receipt
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Ethereum receipt has no status: {receipt}"))?;
    let status = match parse_hex_u64(success)? {
        1 => SubmitStatus::ConfirmedSuccess,
        0 => SubmitStatus::ConfirmedReverted,
        other => bail!("Ethereum receipt has invalid status {other}"),
    };
    let event = match status {
        SubmitStatus::ConfirmedSuccess => SubmissionEvent::ObserveSuccess,
        SubmitStatus::ConfirmedReverted => SubmissionEvent::ObserveRevert,
        SubmitStatus::SubmittedUnknown => unreachable!(),
    };
    let persisted_status = next_submission_status(&record.status, event).ok_or_else(|| {
        anyhow!(
            "cannot persist Ethereum receipt in submission state {}",
            record.status
        )
    })?;
    update_submission(node, &record.doc_id, persisted_status, Some(&receipt)).await?;
    Ok(SubmitReceipt {
        tx_hash: record.tx_hash.clone(),
        status,
        receipt: Some(receipt),
    })
}

#[cfg(test)]
mod tests;
