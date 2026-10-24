use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::error::{EthError, Result};
use super::methods::{
    normalize_method, optional_trailing_block_arity, reject_forbidden, validate_query_methods,
};

pub const ETH_USER_AGENT: &str = concat!("gents-eth/", env!("CARGO_PKG_VERSION"));
pub const ETH_GET_LOGS_MAX_RANGE: u64 = 1000;
const DEFAULT_RPC_TIMEOUT_SECS: i64 = 30;

#[async_trait]
pub trait JsonRpcTransport: Send + Sync {
    async fn post(&self, url: &str, body: &Value) -> Result<Value>;
}

#[derive(Debug, Clone)]
pub struct HttpJsonRpc {
    client: reqwest::Client,
}

impl HttpJsonRpc {
    pub fn new() -> Result<Self> {
        Self::with_timeout(HttpEthRpc::configured_timeout(None)?)
    }

    pub fn with_timeout(timeout: Duration) -> Result<Self> {
        if timeout.is_zero() {
            return Err(EthError::ZeroTimeout);
        }
        let client = reqwest::Client::builder()
            .user_agent(ETH_USER_AGENT)
            .timeout(timeout)
            .build()
            .map_err(EthError::HttpClient)?;
        Ok(Self { client })
    }
}

#[async_trait]
impl JsonRpcTransport for HttpJsonRpc {
    async fn post(&self, url: &str, body: &Value) -> Result<Value> {
        let response = self
            .client
            .post(url)
            .json(body)
            .send()
            .await
            .map_err(|source| EthError::Post {
                url: url.to_string(),
                source,
            })?;
        let status = response.status();
        let text = response.text().await.map_err(|source| EthError::ReadBody {
            url: url.to_string(),
            source,
        })?;
        if status.as_u16() == 403 {
            return Err(EthError::HttpForbidden {
                url: url.to_string(),
            });
        }
        if !status.is_success() {
            return Err(EthError::HttpStatus {
                url: url.to_string(),
                status,
                text,
            });
        }
        serde_json::from_str(&text).map_err(|source| EthError::DecodeBody {
            url: url.to_string(),
            text: text.clone(),
            source,
        })
    }
}

pub struct EthRpcClient<T> {
    rpc_url: String,
    expected_chain_id: u64,
    allowed_methods: BTreeSet<String>,
    cached_chain_id: Mutex<Option<u64>>,
    transport: T,
}

pub type HttpEthRpc = EthRpcClient<HttpJsonRpc>;

impl HttpEthRpc {
    pub fn configured_timeout(seconds: Option<i64>) -> Result<Duration> {
        let seconds = seconds.unwrap_or(DEFAULT_RPC_TIMEOUT_SECS);
        if seconds <= 0 {
            return Err(EthError::ZeroTimeout);
        }
        Ok(Duration::from_secs(seconds as u64))
    }

    pub fn http(
        rpc_url: impl Into<String>,
        expected_chain_id: u64,
        query_methods: &[String],
    ) -> Result<Self> {
        Self::http_with_timeout(
            rpc_url,
            expected_chain_id,
            query_methods,
            Self::configured_timeout(None)?,
        )
    }

    pub fn http_with_timeout(
        rpc_url: impl Into<String>,
        expected_chain_id: u64,
        query_methods: &[String],
        timeout: Duration,
    ) -> Result<Self> {
        Self::new(
            rpc_url,
            expected_chain_id,
            query_methods,
            HttpJsonRpc::with_timeout(timeout)?,
        )
    }
}

impl<T> EthRpcClient<T>
where
    T: JsonRpcTransport,
{
    pub fn new(
        rpc_url: impl Into<String>,
        expected_chain_id: u64,
        query_methods: &[String],
        transport: T,
    ) -> Result<Self> {
        let rpc_url = rpc_url.into();
        if rpc_url.trim().is_empty() {
            return Err(EthError::RpcUrlEmpty);
        }
        if expected_chain_id == 0 {
            return Err(EthError::ChainIdZero);
        }
        let allowed = validate_query_methods(query_methods)?;
        Ok(Self {
            rpc_url,
            expected_chain_id,
            allowed_methods: allowed.into_iter().collect(),
            cached_chain_id: Mutex::new(None),
            transport,
        })
    }

    pub fn user_agent() -> &'static str {
        ETH_USER_AGENT
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let method = normalize_method(method);
        reject_forbidden(&method)?;
        if !self.allowed_methods.contains(&method) {
            return Err(EthError::MethodNotConfigured { method });
        }
        let params = default_block_if_needed(&method, params)?;
        if method == "eth_getLogs" {
            preflight_get_logs(&params)?;
        }
        self.ensure_chain_id().await?;
        self.rpc(&method, params).await
    }

    /// Internal `eth_call` for generated read tools; bypasses `query_methods`.
    pub async fn eth_call(&self, to: &str, data: &str, block: Option<&str>) -> Result<Value> {
        self.ensure_chain_id().await?;
        let block = block
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("latest");
        self.rpc("eth_call", json!([{ "to": to, "data": data }, block]))
            .await
    }

    #[allow(dead_code)]
    pub(crate) async fn simulate_transaction(&self, transaction: Value) -> Result<Value> {
        self.ensure_chain_id().await?;
        self.rpc("eth_call", json!([transaction, "pending"])).await
    }

    #[allow(dead_code)]
    pub(crate) async fn pending_nonce(&self, address: &str) -> Result<Value> {
        self.ensure_chain_id().await?;
        self.rpc("eth_getTransactionCount", json!([address, "pending"]))
            .await
    }

    #[allow(dead_code)]
    pub(crate) async fn estimate_gas(&self, transaction: Value) -> Result<Value> {
        self.ensure_chain_id().await?;
        self.rpc("eth_estimateGas", json!([transaction])).await
    }

    #[allow(dead_code)]
    pub(crate) async fn max_priority_fee_per_gas(&self) -> Result<Value> {
        self.ensure_chain_id().await?;
        self.rpc("eth_maxPriorityFeePerGas", json!([])).await
    }

    #[allow(dead_code)]
    pub(crate) async fn gas_price(&self) -> Result<Value> {
        self.ensure_chain_id().await?;
        self.rpc("eth_gasPrice", json!([])).await
    }

    pub(crate) async fn latest_base_fee(&self) -> Result<u128> {
        self.ensure_chain_id().await?;
        let block = self
            .rpc("eth_getBlockByNumber", json!(["latest", false]))
            .await
            .map_err(|source| EthError::LatestBlock {
                source: Box::new(source),
            })?;
        let fee = block
            .get("baseFeePerGas")
            .and_then(Value::as_str)
            .ok_or_else(|| EthError::BaseFeeMissing {
                block: block.clone(),
            })?;
        let hex = fee
            .strip_prefix("0x")
            .or_else(|| fee.strip_prefix("0X"))
            .ok_or_else(|| EthError::BaseFeeNotHex {
                fee: fee.to_string(),
            })?;
        u128::from_str_radix(hex, 16).map_err(|source| EthError::BaseFeeDecode {
            fee: fee.to_string(),
            source,
        })
    }

    #[allow(dead_code)]
    pub(crate) async fn send_raw_transaction(&self, raw: &str) -> Result<Value> {
        self.ensure_chain_id().await?;
        self.rpc("eth_sendRawTransaction", json!([raw])).await
    }

    #[allow(dead_code)]
    pub(crate) async fn transaction_receipt(&self, tx_hash: &str) -> Result<Value> {
        self.ensure_chain_id().await?;
        self.rpc("eth_getTransactionReceipt", json!([tx_hash]))
            .await
    }

    async fn ensure_chain_id(&self) -> Result<()> {
        if let Some(cached) = *self
            .cached_chain_id
            .lock()
            .expect("eth chain id cache lock poisoned")
        {
            if cached != self.expected_chain_id {
                return Err(EthError::ChainIdCached {
                    expected: self.expected_chain_id,
                    actual: cached,
                });
            }
            return Ok(());
        }
        let result = self.rpc("eth_chainId", json!([])).await?;
        let actual = parse_hex_u64(&value_as_quantity(&result)?).map_err(|source| {
            EthError::DecodeChainId {
                source: Box::new(source),
            }
        })?;
        *self
            .cached_chain_id
            .lock()
            .expect("eth chain id cache lock poisoned") = Some(actual);
        if actual != self.expected_chain_id {
            return Err(EthError::ChainIdMismatch {
                expected: self.expected_chain_id,
                actual,
            });
        }
        Ok(())
    }

    async fn rpc(&self, method: &str, params: Value) -> Result<Value> {
        let params = match params {
            Value::Array(_) => params,
            Value::Null => json!([]),
            other => return Err(EthError::RpcParamsNotArray { got: other }),
        };
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let response = self.transport.post(&self.rpc_url, &body).await?;
        decode_rpc_result(response)
    }
}

fn decode_rpc_result(response: Value) -> Result<Value> {
    if let Some(error) = response.get("error").filter(|error| !error.is_null()) {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("eth JSON-RPC error")
            .to_string();
        let code = error.get("code").cloned().unwrap_or(Value::Null);
        if is_pruned_history_message(&message) {
            return Err(EthError::PrunedHistory { message });
        }
        if let Some(reason) = revert_reason_from_error(error) {
            return Err(EthError::Reverted { reason });
        }
        return Err(EthError::RpcError { code, message });
    }
    response
        .get("result")
        .cloned()
        .ok_or(EthError::MissingResult { response })
}

pub fn revert_reason_from_error(error: &Value) -> Option<String> {
    let data = error.get("data")?;
    let hex = match data {
        Value::String(text) => text.as_str(),
        Value::Object(obj) => obj.get("data").and_then(Value::as_str)?,
        _ => return None,
    };
    decode_revert_data(hex)
}

pub fn decode_revert_data(hex: &str) -> Option<String> {
    let hex = hex.strip_prefix("0x").unwrap_or(hex);
    if hex.len() < 8 + 64 + 64 || !hex.to_ascii_lowercase().starts_with("08c379a0") {
        return None;
    }
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
        .collect::<Option<Vec<_>>>()?;
    if bytes.len() < 4 + 32 + 32 {
        return None;
    }
    let str_len = u32::from_be_bytes(bytes[4 + 32 + 28..4 + 32 + 32].try_into().ok()?) as usize;
    let start: usize = 4 + 64;
    let end = start.checked_add(str_len)?;
    let slice = bytes.get(start..end)?;
    String::from_utf8(slice.to_vec()).ok()
}

pub fn is_pruned_history_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("metadata is not found")
        || lower.contains("missing trie node")
        || lower.contains("historical state")
        || lower.contains("only latest")
        || lower.contains("header not found")
        || lower.contains("block is not available")
        || lower.contains("unknown block")
        || lower.contains("pruned")
}

fn default_block_if_needed(method: &str, params: Value) -> Result<Value> {
    let mut params = match params {
        Value::Null => Vec::new(),
        Value::Array(items) => items,
        other => return Err(EthError::RpcParamsNotArray { got: other }),
    };
    if let Some(arity) = optional_trailing_block_arity(method) {
        if params.len() + 1 == arity {
            params.push(json!("latest"));
        }
    }
    Ok(Value::Array(params))
}

pub fn preflight_get_logs(params: &Value) -> Result<()> {
    let filter = params
        .as_array()
        .and_then(|items| items.first())
        .and_then(Value::as_object)
        .ok_or(EthError::GetLogsFilterMissing)?;

    let address_ok = match filter.get("address") {
        Some(Value::String(value)) if is_eth_address(value) => true,
        Some(Value::Array(values))
            if values
                .iter()
                .any(|item| item.as_str().is_some_and(is_eth_address)) =>
        {
            true
        }
        _ => false,
    };
    let topics_ok = filter.get("topics").is_some_and(topic_filter_constrains);
    if !address_ok && !topics_ok {
        return Err(EthError::GetLogsUnfiltered);
    }

    if let Some(block_hash) = filter.get("blockHash") {
        if filter.contains_key("fromBlock") || filter.contains_key("toBlock") {
            return Err(EthError::GetLogsBlockHashWithRange);
        }
        if !is_block_hash(block_hash) {
            return Err(EthError::GetLogsBlockHashInvalid);
        }
        return Ok(());
    }

    let from = block_tag(filter.get("fromBlock"))?.default_latest();
    let to = block_tag(filter.get("toBlock"))?.default_latest();
    match (from, to) {
        (BlockTag::Earliest, _) | (_, BlockTag::Earliest) => {
            return Err(EthError::GetLogsUnboundedEarliest)
        }
        (BlockTag::Number(from_num), BlockTag::Number(to_num)) => {
            if to_num < from_num {
                return Err(EthError::GetLogsReversedRange);
            }
            let distance = to_num - from_num;
            if distance >= ETH_GET_LOGS_MAX_RANGE {
                return Err(EthError::GetLogsRangeTooLarge {
                    blocks: distance.saturating_add(1),
                });
            }
        }
        (from_tag, to_tag) if from_tag == to_tag => {}
        _ => return Err(EthError::GetLogsMixedRange),
    }
    Ok(())
}

fn topic_filter_constrains(value: &Value) -> bool {
    match value {
        Value::String(topic) => !topic.trim().is_empty(),
        Value::Array(values) => values.iter().any(topic_filter_constrains),
        _ => false,
    }
}

fn is_block_hash(value: &Value) -> bool {
    value.as_str().is_some_and(|value| {
        let hex = value.strip_prefix("0x").unwrap_or(value);
        hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn is_eth_address(value: &str) -> bool {
    let hex = value.strip_prefix("0x").unwrap_or(value);
    hex.len() == 40 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockTag {
    Number(u64),
    Latest,
    Pending,
    Safe,
    Finalized,
    Earliest,
    Missing,
}

impl BlockTag {
    fn default_latest(self) -> Self {
        match self {
            Self::Missing => Self::Latest,
            other => other,
        }
    }
}

fn block_tag(value: Option<&Value>) -> Result<BlockTag> {
    let Some(value) = value else {
        return Ok(BlockTag::Missing);
    };
    match value {
        Value::Null => Ok(BlockTag::Missing),
        Value::String(tag) => parse_block_tag(tag),
        Value::Number(number) => {
            let n = number.as_u64().ok_or_else(|| EthError::BlockNumberNotU64 {
                number: number.clone(),
            })?;
            Ok(BlockTag::Number(n))
        }
        other => Err(EthError::BlockTagType { got: other.clone() }),
    }
}

fn parse_block_tag(tag: &str) -> Result<BlockTag> {
    match tag.trim() {
        "" => Ok(BlockTag::Missing),
        "latest" => Ok(BlockTag::Latest),
        "pending" => Ok(BlockTag::Pending),
        "safe" => Ok(BlockTag::Safe),
        "finalized" => Ok(BlockTag::Finalized),
        "earliest" => Ok(BlockTag::Earliest),
        hex => Ok(BlockTag::Number(parse_hex_u64(hex)?)),
    }
}

fn value_as_quantity(value: &Value) -> Result<String> {
    match value {
        Value::String(text) => Ok(text.clone()),
        Value::Number(number) => Ok(number.to_string()),
        other => Err(EthError::QuantityType { got: other.clone() }),
    }
}

pub fn parse_hex_u64(value: &str) -> Result<u64> {
    let text = value.trim();
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).map_err(|source| EthError::HexQuantity {
            value: value.to_string(),
            source,
        })
    } else {
        text.parse::<u64>().map_err(|source| EthError::Quantity {
            value: value.to_string(),
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_protocol::error_chain::{find_source, ErrorChain};
    use std::collections::VecDeque;
    use std::sync::Arc;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

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
            self.responses
                .lock()
                .expect("responses")
                .pop_front()
                .ok_or(EthError::ScriptExhausted)
        }
    }

    fn client(methods: &[&str], transport: Scripted) -> EthRpcClient<Scripted> {
        EthRpcClient::new(
            "http://127.0.0.1:1",
            8453,
            &methods.iter().map(|m| m.to_string()).collect::<Vec<_>>(),
            transport,
        )
        .expect("client")
    }

    fn ok_result(value: Value) -> Value {
        json!({"jsonrpc":"2.0","id":1,"result": value})
    }

    #[tokio::test]
    async fn unfiltered_logs_rejected_before_wire() {
        let transport = Scripted::new(vec![]);
        let calls = Arc::clone(&transport.calls);
        let rpc = client(&["eth_getLogs"], transport);
        let err = rpc
            .call("eth_getLogs", json!([{"fromBlock":"0x1","toBlock":"0x2"}]))
            .await
            .expect_err("unfiltered");
        assert!(err.to_string().contains("unfiltered"));
        assert!(calls.lock().expect("calls").is_empty());
    }

    #[tokio::test]
    async fn logs_range_over_1000_rejected_before_wire() {
        let transport = Scripted::new(vec![]);
        let calls = Arc::clone(&transport.calls);
        let rpc = client(&["eth_getLogs"], transport);
        let err = rpc
            .call(
                "eth_getLogs",
                json!([{
                    "fromBlock": "0x1",
                    "toBlock": "0x3ea",
                    "address": "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"
                }]),
            )
            .await
            .expect_err("range");
        assert!(err.to_string().contains("exceeds max"));
        assert!(calls.lock().expect("calls").is_empty());
    }

    #[test]
    fn wildcard_topics_do_not_make_a_log_query_filtered() {
        let error = preflight_get_logs(&json!([{
            "fromBlock": "latest",
            "toBlock": "latest",
            "topics": [null]
        }]))
        .expect_err("wildcard topics");
        assert!(error.to_string().contains("unfiltered"));
    }

    #[test]
    fn numeric_to_latest_log_range_is_rejected() {
        let error = preflight_get_logs(&json!([{
            "fromBlock": "0x1",
            "toBlock": "latest",
            "address": "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"
        }]))
        .expect_err("unbounded mixed range");
        assert!(error.to_string().contains("unbounded"));
    }

    #[test]
    fn inclusive_log_range_is_bounded_to_1000_blocks() {
        let address = "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913";
        preflight_get_logs(&json!([{
            "fromBlock": "0x1",
            "toBlock": "0x3e8",
            "address": address
        }]))
        .expect("exactly 1000 blocks");
        assert!(preflight_get_logs(&json!([{
            "fromBlock": "0x1",
            "toBlock": "0x3e9",
            "address": address
        }]))
        .is_err());
    }

    #[tokio::test]
    async fn chain_id_mismatch_fails() {
        let transport = Scripted::new(vec![ok_result(json!("0x1"))]);
        let rpc = client(&["eth_blockNumber"], transport);
        let err = rpc
            .call("eth_blockNumber", json!([]))
            .await
            .expect_err("mismatch");
        assert!(err.to_string().contains("chain_id mismatch"));
    }

    #[tokio::test]
    async fn matching_chain_id_then_method() {
        let transport = Scripted::new(vec![ok_result(json!("0x2105")), ok_result(json!("0x10"))]);
        let rpc = client(&["eth_blockNumber"], transport);
        let result = rpc.call("eth_blockNumber", json!([])).await.expect("call");
        assert_eq!(result, json!("0x10"));
    }

    #[tokio::test]
    async fn send_method_rejected_even_if_listed() {
        let err = match EthRpcClient::new(
            "http://127.0.0.1:1",
            1,
            &["eth_sendRawTransaction".to_string()],
            Scripted::new(vec![]),
        ) {
            Ok(_) => panic!("send method must fail construction"),
            Err(error) => error,
        };
        assert!(err.to_string().contains("not a read-only"));
    }

    #[tokio::test]
    async fn pruned_history_is_typed_failure() {
        let transport = Scripted::new(vec![
            ok_result(json!("0x2105")),
            json!({"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"metadata is not found"}}),
        ]);
        let rpc = client(&["eth_call"], transport);
        let err = rpc
            .call(
                "eth_call",
                json!([{"to":"0x833589fcd6edb6e08f4c7c32d4f71b54bda02913","data":"0x"}, "0x1"]),
            )
            .await
            .expect_err("pruned");
        assert!(err.to_string().contains("pruned-history"));
        assert!(!err.to_string().contains("retry"));
    }

    #[tokio::test]
    async fn omitted_block_defaults_to_latest() {
        let transport = Scripted::new(vec![ok_result(json!("0x2105")), ok_result(json!("0x1"))]);
        let calls = Arc::clone(&transport.calls);
        let rpc = client(&["eth_getBalance"], transport);
        let _ = rpc
            .call(
                "eth_getBalance",
                json!(["0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"]),
            )
            .await
            .expect("call");
        let calls = calls.lock().expect("calls");
        let balance = calls
            .iter()
            .find(|body| body["method"] == "eth_getBalance")
            .expect("balance call");
        assert_eq!(
            balance["params"],
            json!(["0x833589fcd6edb6e08f4c7c32d4f71b54bda02913", "latest"])
        );
    }

    #[test]
    fn rpc_timeout_defaults_and_invalid_limits() {
        assert_eq!(
            HttpEthRpc::configured_timeout(None).unwrap(),
            Duration::from_secs(30)
        );
        assert_eq!(
            HttpEthRpc::configured_timeout(Some(9)).unwrap(),
            Duration::from_secs(9)
        );
        assert!(HttpEthRpc::configured_timeout(Some(0)).is_err());
        assert!(HttpEthRpc::configured_timeout(Some(-1)).is_err());
        assert!(HttpJsonRpc::with_timeout(Duration::ZERO).is_err());
    }

    #[tokio::test]
    async fn configured_http_timeout_bounds_an_unresponsive_rpc() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });
        let rpc = HttpEthRpc::http_with_timeout(
            &url,
            8453,
            &["eth_chainId".into()],
            Duration::from_millis(100),
        )
        .unwrap();
        let result =
            tokio::time::timeout(Duration::from_secs(5), rpc.call("eth_chainId", json!([]))).await;
        server.abort();
        let error = result
            .expect("configured timeout must beat the default 30 seconds")
            .expect_err("server never responds");
        assert!(
            find_source::<reqwest::Error>(&error).is_some_and(reqwest::Error::is_timeout),
            "{}",
            ErrorChain(&error)
        );
    }

    #[tokio::test]
    async fn http_sends_gents_eth_user_agent() {
        let (url, requests) = spawn_json_rpc_server(ok_result(json!("0x2105"))).await;
        let rpc = HttpEthRpc::http(&url, 8453, &["eth_chainId".to_string()]).expect("http client");
        let result = rpc.call("eth_chainId", json!([])).await.expect("call");
        assert_eq!(result, json!("0x2105"));
        let request = requests.lock().expect("requests")[0].clone();
        assert!(
            request
                .to_ascii_lowercase()
                .contains(&format!("user-agent: {ETH_USER_AGENT}").to_ascii_lowercase()),
            "missing User-Agent in {request}"
        );
    }

    async fn spawn_json_rpc_server(body: Value) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock eth rpc");
        let addr = listener.local_addr().expect("addr");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captures = Arc::clone(&requests);
        let payload = serde_json::to_string(&body).expect("json");
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let Ok(request) = read_http_request(&mut stream).await else {
                    continue;
                };
                captures.lock().expect("requests").push(request);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    payload.len(),
                    payload
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        (format!("http://{addr}"), requests)
    }

    async fn read_http_request(stream: &mut TcpStream) -> std::io::Result<String> {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let n = stream.read(&mut chunk).await?;
            if n == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(header_end) = find_bytes(&buf, b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&buf[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        (name.eq_ignore_ascii_case("content-length"))
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                let body_start = header_end + 4;
                while buf.len() < body_start + content_length {
                    let n = stream.read(&mut chunk).await?;
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                break;
            }
        }
        Ok(String::from_utf8_lossy(&buf).to_string())
    }

    fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }
}
