use anyhow::{anyhow, bail, Context, Result};
use defra_node::EmbeddedNode;
use serde_json::{json, Value};

use super::super::rpc::{decode_revert_data, parse_hex_u64, EthRpcClient, JsonRpcTransport};
use super::encoding::hex_encode;
use super::journal::max_in_flight_nonce;
use super::SubmitRequest;

pub(super) async fn simulate<T>(
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

pub(super) async fn next_nonce<T>(
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

pub(super) async fn gas_fees<T>(
    client: &EthRpcClient<T>,
    request: &SubmitRequest,
) -> Result<(u64, u128, u128)>
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
            return Err(super::super::EthRefusal::Policy(format!(
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
            return Err(super::super::EthRefusal::Policy(format!(
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
