//! Journaled simulate-before-sign submission: signed bytes are persisted before
//! broadcast so recovery rebroadcasts the same transaction hash, never a new nonce.

use rapidhash::RapidHashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use alloy_primitives::{keccak256, Address, U256};
use anyhow::{anyhow, bail, Context, Result};
use defra_node::EmbeddedNode;
use serde::Deserialize;
use serde_json::{json, Value};
use sha3::{Digest, Keccak256};

use crate::graphql::escape_graphql_string;

use super::keys::{address_from_secret, sign_prehash_recoverable};
use super::rpc::{decode_revert_data, parse_hex_u64, EthRpcClient, JsonRpcTransport};

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

async fn simulate<T>(
    client: &EthRpcClient<T>,
    request: &SubmitRequest,
    nonce: u64,
    gas_limit: u64,
    max_fee_per_gas: u128,
    max_priority_fee_per_gas: u128,
) -> Result<()>
where
    T: JsonRpcTransport,
{
    let mut tx = transaction_object(request);
    if let Value::Object(map) = &mut tx {
        map.insert("nonce".to_string(), json!(format!("0x{nonce:x}")));
        map.insert("gas".to_string(), json!(format!("0x{gas_limit:x}")));
        map.insert(
            "maxFeePerGas".to_string(),
            json!(format!("0x{max_fee_per_gas:x}")),
        );
        map.insert(
            "maxPriorityFeePerGas".to_string(),
            json!(format!("0x{max_priority_fee_per_gas:x}")),
        );
        map.insert("type".to_string(), json!("0x2"));
    }
    let result = client
        .simulate_transaction(tx)
        .await
        .context("simulating eth transaction")?;
    if let Some(hex) = result.as_str() {
        if let Some(reason) = decode_revert_data(hex) {
            bail!("simulation reverted: {reason}");
        }
    }
    Ok(())
}

async fn next_nonce<T>(
    node: &EmbeddedNode,
    client: &EthRpcClient<T>,
    from: &str,
    chain_id: u64,
) -> Result<u64>
where
    T: JsonRpcTransport,
{
    let pending = transaction_count(client, from).await?;
    let journaled = max_in_flight_nonce(node, from, chain_id).await?;
    Ok(match journaled {
        Some(nonce) => pending.max(nonce.saturating_add(1)),
        None => pending,
    })
}

async fn max_in_flight_nonce(
    node: &EmbeddedNode,
    from: &str,
    chain_id: u64,
) -> Result<Option<u64>> {
    let from = escape_graphql_string(&from.to_ascii_lowercase());
    let query = format!(
        r#"{{
            EthSubmission(filter: {{
                from_address: {{ _eq: "{from}" }},
                chain_id: {{ _eq: {chain_id} }}
            }}) {{ nonce status }}
        }}"#
    );
    let response =
        crate::graphql::graphql_with_transaction_retry(node, &query, "max_in_flight_nonce").await?;
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("EthSubmission"))
        .cloned()
        .unwrap_or(Value::Array(Vec::new()));
    if rows.is_null() {
        return Ok(None);
    }
    #[derive(Deserialize)]
    struct Row {
        nonce: i64,
        status: String,
    }
    let rows: Vec<Row> = serde_json::from_value(rows)?;
    Ok(rows
        .into_iter()
        .filter(|row| row.status == STATUS_PREPARED || row.status == STATUS_SUBMITTED_UNKNOWN)
        .filter_map(|row| u64::try_from(row.nonce).ok())
        .max())
}

async fn transaction_count<T>(client: &EthRpcClient<T>, address: &str) -> Result<u64>
where
    T: JsonRpcTransport,
{
    let result = client
        .pending_nonce(address)
        .await
        .context("eth_getTransactionCount")?;
    let value = result
        .as_str()
        .ok_or_else(|| anyhow!("eth_getTransactionCount returned non-string {result}"))?;
    parse_hex_u64(value).context("decoding nonce")
}

async fn gas_fees<T>(client: &EthRpcClient<T>, request: &SubmitRequest) -> Result<(u64, u128, u128)>
where
    T: JsonRpcTransport,
{
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
            return Err(super::EthRefusal::Policy(format!(
                "estimated gas {gas_limit} exceeds max_gas {cap}"
            ))
            .into());
        }
    }

    let tip = rpc_u128(
        client
            .max_priority_fee_per_gas()
            .await
            .context("eth_maxPriorityFeePerGas")?,
        "eth_maxPriorityFeePerGas",
    )?;
    let base = client
        .latest_base_fee()
        .await
        .context("eth_getBlockByNumber latest baseFeePerGas")?;
    let max_fee = base
        .checked_mul(2)
        .and_then(|buffered| buffered.checked_add(tip))
        .ok_or_else(|| anyhow!("max fee per gas overflow"))?;
    if let Some(cap) = request.caps.max_fee_per_gas {
        if max_fee > cap {
            return Err(super::EthRefusal::Policy(format!(
                "max fee per gas {max_fee} exceeds cap {cap}"
            ))
            .into());
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
    let response =
        crate::graphql::graphql_with_transaction_retry(node, &query, "load_submission").await?;
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

async fn create_submission(
    node: &std::sync::Arc<EmbeddedNode>,
    record: &SubmissionRecord,
) -> Result<()> {
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
    crate::config_client::ConfigAccess::write_local_response(
        node,
        "eth.create_submission",
        &mutation,
    )
    .await?;
    Ok(())
}

async fn update_submission(
    node: &std::sync::Arc<EmbeddedNode>,
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
    crate::config_client::ConfigAccess::write_local_response(
        node,
        "eth.update_submission",
        &mutation,
    )
    .await?;
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
    if !hex.len().is_multiple_of(2) {
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
mod tests;
