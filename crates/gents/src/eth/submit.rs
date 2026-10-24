//! Journaled simulate-before-sign submission: signed bytes are persisted before
//! broadcast so recovery rebroadcasts the same transaction hash, never a new nonce.

use rapidhash::RapidHashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use alloy_primitives::{keccak256, Address, U256};
use defra_node::EmbeddedNode;
use serde::Deserialize;
use serde_json::{json, Value};
use sha3::{Digest, Keccak256};

use crate::graphql::escape_graphql_string;

use super::error::{EthError, Result};
use super::keys::{address_from_secret, sign_prehash_recoverable};
use super::rpc::{decode_revert_data, parse_hex_u64, EthRpcClient, JsonRpcTransport};
use crate::config_client::ConfigClientError;

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

#[derive(Debug, thiserror::Error)]
pub(crate) enum SubmitError {
    #[error(transparent)]
    Eth(#[from] EthError),
    #[error(transparent)]
    Config(#[from] ConfigClientError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("persisting prepared Ethereum submission")]
    Persist(#[source] ConfigClientError),
    #[error("prepared Ethereum submission was not readable after create")]
    SubmissionNotReadable,
    #[error("eth_sendRawTransaction did not return a hash: {value}")]
    SendReturnedNoHash { value: Value },
    #[error("node returned transaction hash {returned}, expected {expected}")]
    NodeHashMismatch { returned: String, expected: String },
    #[error("cannot broadcast Ethereum submission in state {status}")]
    CannotBroadcast { status: String },
    #[error("Ethereum receipt has no status: {receipt}")]
    ReceiptNoStatus { receipt: Value },
    #[error("Ethereum receipt has invalid status {status}")]
    ReceiptInvalidStatus { status: u64 },
    #[error("cannot persist Ethereum receipt in submission state {status}")]
    CannotPersistReceipt { status: String },
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
) -> std::result::Result<SubmitReceipt, SubmitError>
where
    T: JsonRpcTransport,
{
    validate_request(&request)?;
    let derived = address_from_secret(secret).map_err(|source| EthError::SubmitSignerAddress {
        source: Box::new(source),
    })?;
    if !derived.eq_ignore_ascii_case(&request.from) {
        return Err(EthError::SignerMismatch {
            derived,
            from: request.from.clone(),
        }
        .into());
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
        chain_id: i64::try_from(request.chain_id)
            .map_err(|source| EthError::ChainIdExceedsInt { source })?,
        request_hash,
        from_address: request.from.to_ascii_lowercase(),
        nonce: i64::try_from(nonce).map_err(|source| EthError::NonceExceedsInt { source })?,
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
        return Err(SubmitError::Persist(create_error));
    }
    let persisted = load_submission(node, &submission_key)
        .await?
        .ok_or(SubmitError::SubmissionNotReadable)?;
    resume_submission(node, client, persisted, options).await
}

fn validate_request(request: &SubmitRequest) -> Result<()> {
    if request.idempotency_key.trim().is_empty() {
        return Err(EthError::SubmitIdempotencyKeyMissing);
    }
    if request.principal_did.trim().is_empty() {
        return Err(EthError::SubmitPrincipalMissing);
    }
    if request.chain_id == 0 {
        return Err(EthError::SubmitChainIdZero);
    }
    let from: Address = request
        .from
        .parse()
        .map_err(|source| EthError::SubmitFromInvalid { source })?;
    if from == Address::ZERO {
        return Err(EthError::SubmitFromZero);
    }
    if let Some(to) = request.to.as_deref() {
        let _: Address = to
            .parse()
            .map_err(|source| EthError::SubmitToInvalid { source })?;
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
        return Err(EthError::IdempotencyKeyReused {
            idempotency_key: request.idempotency_key.clone(),
        });
    }
    let request_chain_id =
        i64::try_from(request.chain_id).map_err(|source| EthError::ChainIdExceedsInt { source })?;
    if existing.principal_did != request.principal_did.trim()
        || existing.chain_id != request_chain_id
        || !existing.from_address.eq_ignore_ascii_case(&request.from)
    {
        return Err(EthError::PersistedIdentityMismatch);
    }
    let expected_hash = raw_transaction_hash(&existing.raw_transaction)?;
    if !expected_hash.eq_ignore_ascii_case(&existing.tx_hash) {
        return Err(EthError::PersistedHashInconsistent);
    }
    if existing.nonce < 0 {
        return Err(EthError::PersistedNegativeNonce);
    }
    Ok(())
}

async fn resume_submission<T>(
    node: &Arc<EmbeddedNode>,
    client: &EthRpcClient<T>,
    record: SubmissionRecord,
    options: SubmitOptions,
) -> std::result::Result<SubmitReceipt, SubmitError>
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
            .ok_or_else(|| SubmitError::SendReturnedNoHash {
                value: value.clone(),
            })?;
        if !returned.eq_ignore_ascii_case(&record.tx_hash) {
            return Err(SubmitError::NodeHashMismatch {
                returned: returned.to_owned(),
                expected: record.tx_hash.clone(),
            });
        }
    }
    let submitted_status = next_submission_status(&record.status, SubmissionEvent::Broadcast)
        .ok_or_else(|| SubmitError::CannotBroadcast {
            status: record.status.clone(),
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
        .map_err(|source| EthError::DecodePersistedReceipt { source })?;
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
        .map_err(|source| EthError::ReceiptFetch {
            source: Box::new(source),
        })?;
    Ok((!value.is_null()).then_some(value))
}

async fn persist_receipt(
    node: &Arc<EmbeddedNode>,
    record: &SubmissionRecord,
    receipt: Value,
) -> std::result::Result<SubmitReceipt, SubmitError> {
    let success = receipt
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| SubmitError::ReceiptNoStatus {
            receipt: receipt.clone(),
        })?;
    let status = match parse_hex_u64(success)? {
        1 => SubmitStatus::ConfirmedSuccess,
        0 => SubmitStatus::ConfirmedReverted,
        other => return Err(SubmitError::ReceiptInvalidStatus { status: other }),
    };
    let event = match status {
        SubmitStatus::ConfirmedSuccess => SubmissionEvent::ObserveSuccess,
        SubmitStatus::ConfirmedReverted => SubmissionEvent::ObserveRevert,
        SubmitStatus::SubmittedUnknown => unreachable!(),
    };
    let persisted_status = next_submission_status(&record.status, event).ok_or_else(|| {
        SubmitError::CannotPersistReceipt {
            status: record.status.clone(),
        }
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
    let result =
        client
            .simulate_transaction(tx)
            .await
            .map_err(|source| EthError::SimulateTransaction {
                source: Box::new(source),
            })?;
    if let Some(hex) = result.as_str() {
        if let Some(reason) = decode_revert_data(hex) {
            return Err(EthError::SimulationReverted { reason });
        }
    }
    Ok(())
}

async fn next_nonce<T>(
    node: &EmbeddedNode,
    client: &EthRpcClient<T>,
    from: &str,
    chain_id: u64,
) -> std::result::Result<u64, SubmitError>
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
) -> std::result::Result<Option<u64>, SubmitError> {
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
    let result =
        client
            .pending_nonce(address)
            .await
            .map_err(|source| EthError::TransactionCountFetch {
                source: Box::new(source),
            })?;
    let value = result
        .as_str()
        .ok_or_else(|| EthError::TransactionCountNotString {
            result: result.clone(),
        })?;
    parse_hex_u64(value).map_err(|source| EthError::DecodeNonce {
        source: Box::new(source),
    })
}

async fn gas_fees<T>(client: &EthRpcClient<T>, request: &SubmitRequest) -> Result<(u64, u128, u128)>
where
    T: JsonRpcTransport,
{
    let estimate = client
        .estimate_gas(transaction_object(request))
        .await
        .map_err(|source| EthError::EstimateGasFetch {
            source: Box::new(source),
        })?;
    let estimate = estimate
        .as_str()
        .ok_or_else(|| EthError::EstimateGasNotString {
            estimate: estimate.clone(),
        })?;
    let gas_limit = parse_hex_u64(estimate).map_err(|source| EthError::DecodeGasEstimate {
        source: Box::new(source),
    })?;
    if let Some(cap) = request.caps.max_gas {
        if gas_limit > cap {
            return Err(EthError::GasLimitExceedsCap { gas_limit, cap });
        }
    }

    let tip = rpc_u128(
        client
            .max_priority_fee_per_gas()
            .await
            .map_err(|source| EthError::PriorityFeeFetch {
                source: Box::new(source),
            })?,
        "eth_maxPriorityFeePerGas",
    )?;
    let base = client
        .latest_base_fee()
        .await
        .map_err(|source| EthError::BaseFeeFetch {
            source: Box::new(source),
        })?;
    let max_fee = base
        .checked_mul(2)
        .and_then(|buffered| buffered.checked_add(tip))
        .ok_or(EthError::MaxFeeOverflow)?;
    if let Some(cap) = request.caps.max_fee_per_gas {
        if max_fee > cap {
            return Err(EthError::MaxFeeExceedsCap { max_fee, cap });
        }
    }
    Ok((gas_limit, max_fee, tip.min(max_fee)))
}

fn rpc_u128(value: Value, method: &str) -> Result<u128> {
    let text = value.as_str().ok_or_else(|| EthError::RpcU128NotString {
        method: method.to_string(),
        value: value.clone(),
    })?;
    let hex = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .ok_or_else(|| EthError::RpcU128NotHex {
            method: method.to_string(),
            text: text.to_string(),
        })?;
    u128::from_str_radix(hex, 16).map_err(|source| EthError::RpcU128Decode {
        method: method.to_string(),
        text: text.to_string(),
        source,
    })
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
) -> std::result::Result<Option<SubmissionRecord>, SubmitError> {
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
) -> std::result::Result<(), ConfigClientError> {
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
) -> std::result::Result<(), SubmitError> {
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
        .map(|value| {
            value
                .parse::<Address>()
                .map_err(|source| EthError::ParseTo { source })
        })
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
        return Err(EthError::OddHex {
            value: value.to_string(),
        });
    }
    (0..hex.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&hex[index..index + 2], 16)
                .map_err(|error| EthError::InvalidHex { error })
        })
        .collect()
}

pub(crate) fn is_gas_or_fee_cap_error(error: &SubmitError) -> bool {
    matches!(
        error,
        SubmitError::Eth(EthError::GasLimitExceedsCap { .. } | EthError::MaxFeeExceedsCap { .. })
    )
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
    use gents_protocol::error_chain::ErrorChain;
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
                .ok_or(EthError::ScriptExhausted)?;
            if body["method"] == "eth_sendRawTransaction" {
                let raw = body["params"][0]
                    .as_str()
                    .ok_or(EthError::ScriptedSendMissingRaw)?;
                response["result"] = json!(raw_transaction_hash(raw)?);
            }
            Ok(response)
        }
    }

    fn ok(value: Value) -> Value {
        json!({"jsonrpc":"2.0","id":1,"result": value})
    }

    async fn node() -> (tempfile::TempDir, Arc<EmbeddedNode>) {
        let temp = tempfile::tempdir().expect("tempdir");
        let node = Arc::new(
            crate::test_support::node_builder()
                .data_path(temp.path().join("data"))
                .with_storage_backend(defra_node::StorageBackend::Regolith)
                .build()
                .await
                .expect("node"),
        );
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
                max_fee_per_gas: Some(5_000_000_000),
            },
            idempotency_key: key.to_string(),
        }
    }

    fn submission_script(receipt_status: &str) -> Vec<Value> {
        vec![
            ok(json!("0x2105")),
            ok(json!("0x0")),
            ok(json!("0x5208")),
            ok(json!("0x1")),
            ok(json!({"baseFeePerGas": "0x3b9aca00"})),
            ok(json!("0x")),
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
    async fn submitted_unknown_outcome_never_mints_a_new_nonce_or_bytes() {
        let (_temp, node) = node().await;
        let mut script = submission_script("0x1");
        *script.last_mut().unwrap() = ok(Value::Null);
        let transport = Scripted::new(script);
        let first_calls = Arc::clone(&transport.calls);
        let client = EthRpcClient::new("http://127.0.0.1:1", 8453, &[], transport).unwrap();
        let first = submit_transaction(
            &node,
            &client,
            &ANVIL0,
            request("unknown-1"),
            &NonceGate::default(),
            SubmitOptions {
                receipt_attempts: 1,
                receipt_interval: Duration::ZERO,
            },
        )
        .await
        .expect("unknown-outcome submission");
        assert_eq!(first.status, SubmitStatus::SubmittedUnknown);
        assert_eq!(first.receipt, None, "no receipt was ever observed");

        let row = load_submission(&node, "did:key:zAlice:8453:unknown-1")
            .await
            .expect("load")
            .expect("durable submission row");
        assert_eq!(row.status, STATUS_SUBMITTED_UNKNOWN);
        assert_eq!(row.tx_hash, first.tx_hash);

        let original_raw = row.raw_transaction.clone();
        let transport = Scripted::new(vec![
            ok(json!("0x2105")),
            ok(Value::Null),
            ok(json!({})),
            ok(json!({"status": "0x1"})),
        ]);
        let recovery_calls = Arc::clone(&transport.calls);
        let client = EthRpcClient::new("http://127.0.0.1:1", 8453, &[], transport).unwrap();
        let second = submit_transaction(
            &node,
            &client,
            &ANVIL0,
            request("unknown-1"),
            &NonceGate::default(),
            SubmitOptions {
                receipt_attempts: 1,
                receipt_interval: Duration::ZERO,
            },
        )
        .await
        .expect("recovered submission");
        assert_eq!(second.tx_hash, first.tx_hash, "same bytes recovered");
        assert_eq!(second.status, SubmitStatus::ConfirmedSuccess);

        let row = load_submission(&node, "did:key:zAlice:8453:unknown-1")
            .await
            .expect("load")
            .expect("durable submission row");
        assert_eq!(row.status, STATUS_CONFIRMED_SUCCESS);
        assert_eq!(row.nonce, 0, "recovery reuses the journaled nonce");
        assert_eq!(row.raw_transaction, original_raw);
        for calls in [&first_calls, &recovery_calls] {
            let calls = calls.lock().unwrap();
            let sends: Vec<_> = calls
                .iter()
                .filter(|call| call["method"] == "eth_sendRawTransaction")
                .collect();
            assert_eq!(sends.len(), 1);
            assert_eq!(sends[0]["params"][0].as_str(), Some(original_raw.as_str()));
        }
        assert!(recovery_calls
            .lock()
            .unwrap()
            .iter()
            .all(|call| call["method"] != "eth_getTransactionCount"));
    }

    #[tokio::test]
    async fn reused_idempotency_key_for_a_different_request_is_rejected() {
        let (_temp, node) = node().await;
        let transport = Scripted::new(submission_script("0x1"));
        let calls = Arc::clone(&transport.calls);
        let client = EthRpcClient::new("http://127.0.0.1:1", 8453, &[], transport).unwrap();
        let first = submit_transaction(
            &node,
            &client,
            &ANVIL0,
            request("conflict-1"),
            &NonceGate::default(),
            SubmitOptions {
                receipt_attempts: 1,
                receipt_interval: Duration::ZERO,
            },
        )
        .await
        .expect("first submission");
        assert_eq!(first.status, SubmitStatus::ConfirmedSuccess);

        let before_calls = calls.lock().unwrap().len();
        let before = load_submission(&node, "did:key:zAlice:8453:conflict-1")
            .await
            .unwrap()
            .unwrap();
        let mut conflicting = request("conflict-1");
        conflicting.to = Some("0x0000000000000000000000000000000000000001".to_string());
        let error = submit_transaction(
            &node,
            &client,
            &ANVIL0,
            conflicting,
            &NonceGate::default(),
            SubmitOptions {
                receipt_attempts: 1,
                receipt_interval: Duration::ZERO,
            },
        )
        .await
        .expect_err("conflicting reuse must be rejected");
        let message = error.to_string();
        assert!(
            message.contains("already used for a different Ethereum request"),
            "{message}"
        );

        let sends = calls
            .lock()
            .unwrap()
            .iter()
            .filter(|body| body["method"] == "eth_sendRawTransaction")
            .count();
        assert_eq!(sends, 1);
        assert_eq!(calls.lock().unwrap().len(), before_calls);
        let row = load_submission(&node, "did:key:zAlice:8453:conflict-1")
            .await
            .expect("load")
            .expect("durable submission row");
        assert_eq!(row.status, STATUS_CONFIRMED_SUCCESS);
        assert_eq!(row.request_hash, before.request_hash);
        assert_eq!(row.raw_transaction, before.raw_transaction);
        assert_eq!(
            row.from_address,
            address_from_secret(&ANVIL0)
                .expect("from")
                .to_ascii_lowercase()
        );
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

    #[tokio::test]
    async fn signing_key_must_match_from_address() {
        let (_temp, node) = node().await;
        let transport = Scripted::new(vec![ok(json!("0x2105"))]);
        let client = EthRpcClient::new("http://127.0.0.1:1", 8453, &[], transport).unwrap();
        let other = [0x11u8; 32];
        let error = submit_transaction(
            &node,
            &client,
            &other,
            request("mismatch"),
            &NonceGate::default(),
            SubmitOptions {
                receipt_attempts: 1,
                receipt_interval: Duration::ZERO,
            },
        )
        .await
        .expect_err("mismatched signer");
        assert!(
            error.to_string().contains("does not match"),
            "{}",
            ErrorChain(&error)
        );
    }

    #[tokio::test]
    async fn in_flight_journal_nonce_is_not_reused() {
        let (_temp, node) = node().await;
        let from = address_from_secret(&ANVIL0).expect("from");
        let prepared = SubmissionRecord {
            doc_id: String::new(),
            submission_key: "did:key:zAlice:8453:held".to_string(),
            principal_did: "did:key:zAlice".to_string(),
            chain_id: 8453,
            request_hash: "0xabc".to_string(),
            from_address: from.to_ascii_lowercase(),
            nonce: 0,
            raw_transaction: "0x02dead".to_string(),
            tx_hash: "0x1111".to_string(),
            status: STATUS_PREPARED.to_string(),
            receipt_json: None,
        };
        create_submission(&node, &prepared)
            .await
            .expect("persist in-flight nonce 0");

        let transport = Scripted::new(submission_script("0x1"));
        let client = EthRpcClient::new("http://127.0.0.1:1", 8453, &[], transport).unwrap();
        submit_transaction(
            &node,
            &client,
            &ANVIL0,
            request("second-key"),
            &NonceGate::default(),
            SubmitOptions {
                receipt_attempts: 1,
                receipt_interval: Duration::ZERO,
            },
        )
        .await
        .expect("second key");

        let loaded = load_submission(&node, "did:key:zAlice:8453:second-key")
            .await
            .expect("load")
            .expect("second submission");
        assert_eq!(
            loaded.nonce, 1,
            "in-flight prepared nonce 0 must bump the next key to 1"
        );
    }

    #[tokio::test]
    async fn eip1559_fees_are_twice_base_fee_plus_tip() {
        let transport = Scripted::new(vec![
            ok(json!("0x2105")),
            ok(json!("0x5208")),
            ok(json!("0x1")),
            ok(json!({"baseFeePerGas": "0x64"})),
        ]);
        let client = EthRpcClient::new("http://127.0.0.1:1", 8453, &[], transport).unwrap();
        let (gas_limit, max_fee, tip) = gas_fees(&client, &request("fees")).await.expect("fees");
        assert_eq!(gas_limit, 0x5208);
        assert_eq!(tip, 1);
        assert_eq!(max_fee, 2 * 0x64 + 1);
    }

    #[tokio::test]
    async fn simulate_call_includes_estimated_gas() {
        let (_temp, node) = node().await;
        let transport = Scripted::new(submission_script("0x1"));
        let calls = Arc::clone(&transport.calls);
        let client = EthRpcClient::new("http://127.0.0.1:1", 8453, &[], transport).unwrap();
        submit_transaction(
            &node,
            &client,
            &ANVIL0,
            request("sim-gas"),
            &NonceGate::default(),
            SubmitOptions {
                receipt_attempts: 1,
                receipt_interval: Duration::ZERO,
            },
        )
        .await
        .expect("submit");
        let simulate = calls
            .lock()
            .unwrap()
            .iter()
            .find(|body| body["method"] == "eth_call")
            .cloned()
            .expect("eth_call");
        let tx = &simulate["params"][0];
        assert!(
            tx.get("gas").and_then(Value::as_str).is_some(),
            "simulate must pass estimated gas: {tx}"
        );
    }

    #[tokio::test]
    async fn fee_cap_is_fail_closed() {
        let transport = Scripted::new(vec![
            ok(json!("0x2105")),
            ok(json!("0x5208")),
            ok(json!("0x1")),
            ok(json!({"baseFeePerGas": "0x3b9aca00"})),
        ]);
        let client = EthRpcClient::new("http://127.0.0.1:1", 8453, &[], transport).unwrap();
        let mut capped = request("capped");
        capped.caps.max_fee_per_gas = Some(100);
        let error = SubmitError::from(gas_fees(&client, &capped).await.expect_err("cap"));
        assert!(
            matches!(error, SubmitError::Eth(EthError::MaxFeeExceedsCap { .. }))
                && is_gas_or_fee_cap_error(&error),
            "{}",
            ErrorChain(&error)
        );
    }
}
