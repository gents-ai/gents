use super::*;
use crate::ensure_runtime_schemas;
use crate::eth::keys::address_from_secret;
use crate::eth::rpc::decode_revert_data;
use async_trait::async_trait;
use std::collections::BTreeSet;
use std::collections::VecDeque;

const ANVIL0: [u8; 32] = [
    0xac, 0x09, 0x74, 0xbe, 0xc3, 0x9a, 0x17, 0xe3, 0x6b, 0xa4, 0xa6, 0xb4, 0xd2, 0x38, 0xff, 0x94,
    0x4b, 0xac, 0xb4, 0x78, 0xcb, 0xed, 0x5e, 0xfc, 0xae, 0x78, 0x4d, 0x7b, 0xf4, 0xf2, 0xff, 0x80,
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
        "{message:#}"
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
    assert!(error.to_string().contains("does not match"), "{error:#}");
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
    let error = gas_fees(&client, &capped).await.expect_err("cap");
    assert!(error.to_string().contains("exceeds cap"), "{error:#}");
}
