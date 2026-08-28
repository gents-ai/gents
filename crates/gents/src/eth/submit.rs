//! Durable simulate-before-sign Ethereum submission.
//!
//! The exact signed bytes are journaled before broadcast. Recovery therefore
//! polls or rebroadcasts the same transaction hash instead of choosing a new
//! nonce and repeating the action.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use alloy_primitives::{keccak256, Address, U256};
use anyhow::{anyhow, bail, Context, Result};
use defra_node::EmbeddedNode;
use serde::Deserialize;
use serde_json::{json, Value};
use sha3::{Digest, Keccak256};

use crate::graphql::{escape_graphql_string, graphql_mutation_with_transaction_retry};

use super::keys::sign_prehash_recoverable;
use super::rpc::{parse_hex_u64, EthRpcClient, JsonRpcTransport};

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

#[derive(Debug, Default)]
pub(crate) struct NonceGate {
    inner: Mutex<HashMap<(String, u64), Arc<tokio::sync::Mutex<()>>>>,
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

pub(crate) async fn submit_transaction<T: JsonRpcTransport>(
    node: &EmbeddedNode,
    client: &EthRpcClient<T>,
    secret: &[u8; 32],
    request: SubmitRequest,
    nonces: &NonceGate,
    options: SubmitOptions,
) -> Result<SubmitReceipt> {
    validate_request(&request)?;
    let submission_key = submission_key(&request);
    let request_hash = request_hash(&request)?;
    let nonce_slot = nonces.slot(&request.from, request.chain_id);
    let _nonce_lock = nonce_slot.lock().await;
    if let Some(existing) = load_submission(node, &submission_key).await? {
        verify_existing(&existing, &request, &request_hash)?;
        return resume_submission(node, client, existing, options).await;
    }

    simulate(client, &request).await?;
    let nonce = transaction_count(client, &request.from).await?;
    let (gas_limit, max_fee_per_gas, max_priority_fee_per_gas) = gas_fees(client, &request).await?;
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
        principal_did: request.principal_did.clone(),
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

async fn resume_submission<T: JsonRpcTransport>(
    node: &EmbeddedNode,
    client: &EthRpcClient<T>,
    record: SubmissionRecord,
    options: SubmitOptions,
) -> Result<SubmitReceipt> {
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

    // A transport error after broadcast is an unknown outcome, not permission
    // to choose a new nonce. The durable row keeps recovery on these bytes.
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

async fn fetch_receipt<T: JsonRpcTransport>(
    client: &EthRpcClient<T>,
    tx_hash: &str,
) -> Result<Option<Value>> {
    let value = client
        .transaction_receipt(tx_hash)
        .await
        .context("eth_getTransactionReceipt")?;
    Ok((!value.is_null()).then_some(value))
}

async fn persist_receipt(
    node: &EmbeddedNode,
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

async fn simulate<T: JsonRpcTransport>(
    client: &EthRpcClient<T>,
    request: &SubmitRequest,
) -> Result<()> {
    client
        .simulate_transaction(transaction_object(request))
        .await
        .context("simulating eth transaction")?;
    Ok(())
}

async fn transaction_count<T: JsonRpcTransport>(
    client: &EthRpcClient<T>,
    address: &str,
) -> Result<u64> {
    let result = client
        .pending_nonce(address)
        .await
        .context("eth_getTransactionCount")?;
    let value = result
        .as_str()
        .ok_or_else(|| anyhow!("eth_getTransactionCount returned non-string {result}"))?;
    parse_hex_u64(value).context("decoding nonce")
}

async fn gas_fees<T: JsonRpcTransport>(
    client: &EthRpcClient<T>,
    request: &SubmitRequest,
) -> Result<(u64, u128, u128)> {
    let estimate = client
        .estimate_gas(transaction_object(request))
        .await
        .context("eth_estimateGas")?;
    let estimate = estimate
        .as_str()
        .ok_or_else(|| anyhow!("eth_estimateGas returned non-string {estimate}"))?;
    let gas_limit = parse_hex_u64(estimate).context("decoding gas estimate")?;
    if let Some(cap) = request.caps.max_gas {
        if gas_limit > cap {
            bail!("estimated gas {gas_limit} exceeds max_gas {cap}");
        }
    }

    let tip = rpc_u128(
        client
            .max_priority_fee_per_gas()
            .await
            .context("eth_maxPriorityFeePerGas")?,
        "eth_maxPriorityFeePerGas",
    )?;
    let base = rpc_u128(
        client.gas_price().await.context("eth_gasPrice")?,
        "eth_gasPrice",
    )?;
    let max_fee = base
        .checked_add(tip)
        .ok_or_else(|| anyhow!("max fee per gas overflow"))?;
    if let Some(cap) = request.caps.max_fee_per_gas {
        if max_fee > cap {
            bail!("max fee per gas {max_fee} exceeds cap {cap}");
        }
    }
    Ok((gas_limit, max_fee, tip.min(max_fee)))
}

fn rpc_u128(value: Value, method: &str) -> Result<u128> {
    let text = value
        .as_str()
        .ok_or_else(|| anyhow!("{method} returned non-string {value}"))?;
    let hex = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .ok_or_else(|| anyhow!("{method} returned non-hex quantity {text:?}"))?;
    u128::from_str_radix(hex, 16).with_context(|| format!("decoding {method} result {text}"))
}

fn transaction_object(request: &SubmitRequest) -> Value {
    let mut tx = serde_json::Map::new();
    tx.insert("from".to_string(), json!(request.from));
    if let Some(to) = &request.to {
        tx.insert("to".to_string(), json!(to));
    }
    if !request.data.is_empty() {
        tx.insert(
            "data".to_string(),
            json!(format!("0x{}", hex_encode(&request.data))),
        );
    }
    if !request.value.is_zero() {
        tx.insert("value".to_string(), json!(format!("0x{:x}", request.value)));
    }
    Value::Object(tx)
}

fn raw_transaction_hash(raw_transaction: &str) -> Result<String> {
    let bytes = decode_hex(raw_transaction)?;
    Ok(format!("{:#x}", keccak256(bytes)))
}

async fn load_submission(
    node: &EmbeddedNode,
    submission_key: &str,
) -> Result<Option<SubmissionRecord>> {
    let key = escape_graphql_string(submission_key);
    let query = format!(
        r#"{{
            EthSubmission(filter: {{ submission_key: {{ _eq: "{key}" }} }}, limit: 1) {{
                _docID submission_key principal_did chain_id request_hash from_address nonce
                raw_transaction tx_hash status receipt_json
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        bail!("load EthSubmission failed: {:?}", response.errors);
    }
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("EthSubmission"))
        .cloned()
        .unwrap_or(Value::Array(Vec::new()));
    if rows.is_null() {
        return Ok(None);
    }
    let mut rows: Vec<SubmissionRecord> = serde_json::from_value(rows)?;
    Ok(rows.pop())
}

async fn create_submission(node: &EmbeddedNode, record: &SubmissionRecord) -> Result<()> {
    let submission_key = escape_graphql_string(&record.submission_key);
    let principal_did = escape_graphql_string(&record.principal_did);
    let request_hash = escape_graphql_string(&record.request_hash);
    let from_address = escape_graphql_string(&record.from_address);
    let raw_transaction = escape_graphql_string(&record.raw_transaction);
    let tx_hash = escape_graphql_string(&record.tx_hash);
    let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            create_EthSubmission(input: {{
                submission_key: "{submission_key}",
                principal_did: "{principal_did}",
                chain_id: {},
                request_hash: "{request_hash}",
                from_address: "{from_address}",
                nonce: {},
                raw_transaction: "{raw_transaction}",
                tx_hash: "{tx_hash}",
                status: "{STATUS_PREPARED}",
                receipt_json: null,
                created_at: "{now}",
                updated_at: "{now}"
            }}) {{ _docID }}
        }}"#,
        record.chain_id, record.nonce
    );
    graphql_mutation_with_transaction_retry(node, &mutation, "create EthSubmission").await?;
    Ok(())
}

async fn update_submission(
    node: &EmbeddedNode,
    doc_id: &str,
    status: &str,
    receipt: Option<&Value>,
) -> Result<()> {
    let doc_id = escape_graphql_string(doc_id);
    let status = escape_graphql_string(status);
    let receipt = match receipt {
        Some(value) => format!(
            "\"{}\"",
            escape_graphql_string(&serde_json::to_string(value)?)
        ),
        None => "null".to_string(),
    };
    let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            update_EthSubmission(
                docID: "{doc_id}",
                input: {{ status: "{status}", receipt_json: {receipt}, updated_at: "{now}" }}
            ) {{ _docID }}
        }}"#
    );
    graphql_mutation_with_transaction_retry(node, &mutation, "update EthSubmission").await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn sign_eip1559(
    secret: &[u8; 32],
    chain_id: u64,
    nonce: u64,
    gas_limit: u64,
    max_fee_per_gas: u128,
    max_priority_fee_per_gas: u128,
    to: Option<&str>,
    value: U256,
    data: &[u8],
) -> Result<String> {
    let to = to
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.parse::<Address>().context("parsing tx to"))
        .transpose()?;
    let unsigned_fields = vec![
        rlp_u64(chain_id),
        rlp_u64(nonce),
        rlp_u128(max_priority_fee_per_gas),
        rlp_u128(max_fee_per_gas),
        rlp_u64(gas_limit),
        rlp_bytes(
            to.as_ref()
                .map(|address| address.as_slice())
                .unwrap_or_default(),
        ),
        rlp_u256(value),
        rlp_bytes(data),
        rlp_list(&[]),
    ];
    let unsigned = typed_transaction(&unsigned_fields);
    let hash = keccak256(&unsigned);
    let mut digest = [0u8; 32];
    digest.copy_from_slice(hash.as_slice());
    let (r, s, y_parity) = sign_prehash_recoverable(secret, &digest)?;

    let mut signed_fields = unsigned_fields;
    signed_fields.extend([
        rlp_u64(u64::from(y_parity)),
        rlp_integer_bytes(&r),
        rlp_integer_bytes(&s),
    ]);
    Ok(format!(
        "0x{}",
        hex_encode(&typed_transaction(&signed_fields))
    ))
}

fn typed_transaction(fields: &[Vec<u8>]) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(fields.iter().map(Vec::len).sum::<usize>() + 4);
    encoded.push(0x02);
    encoded.extend(rlp_list(fields));
    encoded
}

fn rlp_u64(value: u64) -> Vec<u8> {
    rlp_integer_bytes(&value.to_be_bytes())
}

fn rlp_u128(value: u128) -> Vec<u8> {
    rlp_integer_bytes(&value.to_be_bytes())
}

fn rlp_u256(value: U256) -> Vec<u8> {
    rlp_integer_bytes(&value.to_be_bytes::<32>())
}

fn rlp_integer_bytes(bytes: &[u8]) -> Vec<u8> {
    let first = bytes
        .iter()
        .position(|byte| *byte != 0)
        .unwrap_or(bytes.len());
    rlp_bytes(&bytes[first..])
}

fn rlp_bytes(bytes: &[u8]) -> Vec<u8> {
    if bytes.len() == 1 && bytes[0] < 0x80 {
        return bytes.to_vec();
    }
    let mut encoded = rlp_length_prefix(bytes.len(), 0x80, 0xb7);
    encoded.extend_from_slice(bytes);
    encoded
}

fn rlp_list(fields: &[Vec<u8>]) -> Vec<u8> {
    let payload_len = fields.iter().map(Vec::len).sum();
    let mut encoded = rlp_length_prefix(payload_len, 0xc0, 0xf7);
    for field in fields {
        encoded.extend_from_slice(field);
    }
    encoded
}

fn rlp_length_prefix(length: usize, short_offset: u8, long_offset: u8) -> Vec<u8> {
    if length <= 55 {
        return vec![short_offset + length as u8];
    }
    let bytes = length.to_be_bytes();
    let first = bytes
        .iter()
        .position(|byte| *byte != 0)
        .unwrap_or(bytes.len());
    let length_bytes = &bytes[first..];
    let mut prefix = Vec::with_capacity(length_bytes.len() + 1);
    prefix.push(long_offset + length_bytes.len() as u8);
    prefix.extend_from_slice(length_bytes);
    prefix
}

fn decode_hex(value: &str) -> Result<Vec<u8>> {
    let hex = value.strip_prefix("0x").unwrap_or(value).trim();
    if hex.len() % 2 != 0 {
        bail!("odd-length hex {value}");
    }
    (0..hex.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&hex[index..index + 2], 16)
                .map_err(|error| anyhow!("invalid hex: {error}"))
        })
        .collect()
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ensure_runtime_schemas;
    use crate::eth::keys::address_from_secret;
    use crate::eth::rpc::decode_revert_data;
    use async_trait::async_trait;
    use std::collections::BTreeSet;
    use std::collections::VecDeque;

    const ANVIL0: [u8; 32] = [
        0xac, 0x09, 0x74, 0xbe, 0xc3, 0x9a, 0x17, 0xe3, 0x6b, 0xa4, 0xa6, 0xb4, 0xd2, 0x38, 0xff,
        0x94, 0x4b, 0xac, 0xb4, 0x78, 0xcb, 0xed, 0x5e, 0xfc, 0xae, 0x78, 0x4d, 0x7b, 0xf4, 0xf2,
        0xff, 0x80,
    ];

    #[derive(Clone)]
    struct Scripted {
        responses: Arc<Mutex<VecDeque<Value>>>,
        calls: Arc<Mutex<Vec<Value>>>,
    }

    impl Scripted {
        fn new(responses: Vec<Value>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl JsonRpcTransport for Scripted {
        async fn post(&self, _url: &str, body: &Value) -> Result<Value> {
            self.calls.lock().expect("calls").push(body.clone());
            let mut response = self
                .responses
                .lock()
                .expect("responses")
                .pop_front()
                .ok_or_else(|| anyhow!("scripted eth RPC has no remaining responses"))?;
            if body["method"] == "eth_sendRawTransaction" {
                let raw = body["params"][0]
                    .as_str()
                    .ok_or_else(|| anyhow!("send has no raw transaction"))?;
                response["result"] = json!(raw_transaction_hash(raw)?);
            }
            Ok(response)
        }
    }

    fn ok(value: Value) -> Value {
        json!({"jsonrpc":"2.0","id":1,"result": value})
    }

    async fn node() -> (tempfile::TempDir, EmbeddedNode) {
        let temp = tempfile::tempdir().expect("tempdir");
        let node = EmbeddedNode::builder()
            .data_path(temp.path().join("data"))
            .with_storage_backend(defra_node::StorageBackend::Lark)
            .build()
            .await
            .expect("node");
        ensure_runtime_schemas(&node).await.expect("schemas");
        (temp, node)
    }

    fn request(key: &str) -> SubmitRequest {
        SubmitRequest {
            principal_did: "did:key:zAlice".to_string(),
            chain_id: 8453,
            from: address_from_secret(&ANVIL0).expect("from"),
            to: Some("0x833589fcd6edb6e08f4c7c32d4f71b54bda02913".to_string()),
            value: U256::ZERO,
            data: vec![0x70, 0xa0, 0x82, 0x31],
            caps: GasCaps {
                max_gas: Some(100_000),
                max_fee_per_gas: Some(2_000_000_000),
            },
            idempotency_key: key.to_string(),
        }
    }

    fn submission_script(receipt_status: &str) -> Vec<Value> {
        vec![
            ok(json!("0x2105")),
            ok(json!("0x")),
            ok(json!("0x0")),
            ok(json!("0x5208")),
            ok(json!("0x1")),
            ok(json!("0x3b9aca00")),
            ok(Value::Null),
            ok(Value::Null),
            ok(json!({"status":receipt_status})),
        ]
    }

    #[test]
    fn decode_error_string_revert() {
        let data = "0x08c379a000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000003666f6f0000000000000000000000000000000000000000000000000000000000";
        assert_eq!(decode_revert_data(data).as_deref(), Some("foo"));
    }

    #[test]
    fn signed_raw_tx_hash_is_stable() {
        let raw = sign_eip1559(
            &ANVIL0,
            8453,
            0,
            21_000,
            1_000_000_000,
            1,
            Some("0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"),
            U256::ZERO,
            &[],
        )
        .expect("sign");
        assert_eq!(
            raw,
            "0x02f8688221058001843b9aca0082520894833589fcd6edb6e08f4c7c32d4f71b54bda029138080c001a0f10f51a937121ba378f768d70fdc9a3a77e0def314e26abb2c132877bccaf02ba00d0e6e7d04c4403336985015cd981799f0a7aab7cc60bf81b5b795f1e7748348"
        );
        assert!(raw.starts_with("0x02"), "{raw}");
        assert_eq!(
            raw_transaction_hash(&raw).unwrap(),
            raw_transaction_hash(&raw).unwrap()
        );
    }

    #[test]
    fn transition_table_matches_lean_contract() {
        let machine = crate::lean_vocab_test::lean_state_machine_contract("EthSubmission");
        crate::lean_vocab_test::assert_state_machine_contract_is_complete("EthSubmission");
        let states = [
            STATUS_PREPARED,
            STATUS_SUBMITTED_UNKNOWN,
            STATUS_CONFIRMED_SUCCESS,
            STATUS_CONFIRMED_REVERTED,
        ];
        let events = [
            SubmissionEvent::Broadcast,
            SubmissionEvent::ObserveSuccess,
            SubmissionEvent::ObserveRevert,
        ];
        let actual = states
            .into_iter()
            .flat_map(|state| {
                events.into_iter().filter_map(move |event| {
                    next_submission_status(state, event)
                        .map(|next| (state.to_string(), next.to_string()))
                })
            })
            .collect::<BTreeSet<_>>();
        let expected = machine
            .legal_transitions
            .iter()
            .map(|transition| (transition.from.clone(), transition.to.clone()))
            .collect::<BTreeSet<_>>();
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn prepared_submission_reuses_signed_bytes_after_retry() {
        let (_temp, node) = node().await;
        let transport = Scripted::new(submission_script("0x1"));
        let calls = Arc::clone(&transport.calls);
        let client = EthRpcClient::new("http://127.0.0.1:1", 8453, &[], transport).unwrap();
        let options = SubmitOptions {
            receipt_attempts: 1,
            receipt_interval: Duration::ZERO,
        };
        let first = submit_transaction(
            &node,
            &client,
            &ANVIL0,
            request("tool-call-1"),
            &NonceGate::default(),
            options.clone(),
        )
        .await
        .expect("submit");
        assert_eq!(first.status, SubmitStatus::ConfirmedSuccess);

        let second = submit_transaction(
            &node,
            &client,
            &ANVIL0,
            request("tool-call-1"),
            &NonceGate::default(),
            options,
        )
        .await
        .expect("durable retry");
        assert_eq!(second.tx_hash, first.tx_hash);
        let sends = calls
            .lock()
            .unwrap()
            .iter()
            .filter(|body| body["method"] == "eth_sendRawTransaction")
            .count();
        assert_eq!(sends, 1);
    }

    #[tokio::test]
    async fn mined_revert_is_not_reported_as_success() {
        let (_temp, node) = node().await;
        let transport = Scripted::new(submission_script("0x0"));
        let client = EthRpcClient::new("http://127.0.0.1:1", 8453, &[], transport).unwrap();
        let receipt = submit_transaction(
            &node,
            &client,
            &ANVIL0,
            request("reverted-1"),
            &NonceGate::default(),
            SubmitOptions {
                receipt_attempts: 1,
                receipt_interval: Duration::ZERO,
            },
        )
        .await
        .expect("mined revert");
        assert_eq!(receipt.status, SubmitStatus::ConfirmedReverted);
    }
}
