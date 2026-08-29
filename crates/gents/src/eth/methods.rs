//! Builtin read-only JSON-RPC ceiling for `query_methods`.
//!
//! Operator allowlists are intersected with this set. Send/sign/debug methods
//! are rejected even if listed.

use std::collections::BTreeSet;

use anyhow::{bail, Result};

/// Read-only methods safe to expose against a pruned public endpoint.
pub const BUILTIN_QUERY_METHODS: &[&str] = &[
    "eth_chainId",
    "eth_blockNumber",
    "eth_syncing",
    "eth_gasPrice",
    "eth_maxPriorityFeePerGas",
    "eth_feeHistory",
    "eth_getBalance",
    "eth_getTransactionCount",
    "eth_getCode",
    "eth_getStorageAt",
    "eth_call",
    "eth_estimateGas",
    "eth_getBlockByNumber",
    "eth_getBlockByHash",
    "eth_getBlockReceipts",
    "eth_getTransactionByHash",
    "eth_getTransactionByBlockHashAndIndex",
    "eth_getTransactionByBlockNumberAndIndex",
    "eth_getTransactionReceipt",
    "eth_getLogs",
    "web3_clientVersion",
    "net_version",
];

const FORBIDDEN_EXACT: &[&str] = &[
    "eth_sendRawTransaction",
    "eth_sendTransaction",
    "eth_sign",
    "eth_signTransaction",
    "eth_accounts",
    "eth_subscribe",
    "eth_unsubscribe",
];

/// Normalize and intersect configured methods with the builtin ceiling.
///
/// Empty input is deny-all (returns empty). Unknown or send/sign methods fail.
pub fn validate_query_methods(configured: &[String]) -> Result<Vec<String>> {
    let ceiling: BTreeSet<&str> = BUILTIN_QUERY_METHODS.iter().copied().collect();
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for raw in configured {
        let method = normalize_method(raw);
        if method.is_empty() {
            continue;
        }
        reject_forbidden(&method)?;
        if !ceiling.contains(method.as_str()) {
            bail!("query method {method} is not in the builtin read-only ceiling");
        }
        if seen.insert(method.clone()) {
            out.push(method);
        }
    }
    Ok(out)
}

/// True when `method` is in the intersection of `configured` and the ceiling.
pub fn method_permitted(method: &str, configured: &[String]) -> Result<bool> {
    let allowed = validate_query_methods(configured)?;
    Ok(allowed.iter().any(|item| item == &normalize_method(method)))
}

pub fn normalize_method(method: &str) -> String {
    method.trim().to_string()
}

pub fn reject_forbidden(method: &str) -> Result<()> {
    let method = normalize_method(method);
    if FORBIDDEN_EXACT.contains(&method.as_str())
        || method.starts_with("eth_signTypedData")
        || method.starts_with("debug_")
        || method.starts_with("trace_")
        || method.starts_with("eth_send")
        || method.starts_with("eth_sign")
    {
        bail!("query method {method} is not a read-only JSON-RPC method");
    }
    Ok(())
}

/// Methods whose last argument is an optional block tag.
/// Value is the full arity including the block.
pub fn optional_trailing_block_arity(method: &str) -> Option<usize> {
    match normalize_method(method).as_str() {
        "eth_getBalance" | "eth_getTransactionCount" | "eth_getCode" | "eth_call" => Some(2),
        "eth_getStorageAt" => Some(3),
        "eth_estimateGas" => Some(2),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_configured_is_deny_all() {
        assert!(validate_query_methods(&[]).expect("ok").is_empty());
    }

    #[test]
    fn ceiling_keeps_reads_and_rejects_send() {
        let allowed =
            validate_query_methods(&["eth_chainId".to_string(), "eth_getBalance".to_string()])
                .expect("ok");
        assert_eq!(allowed, vec!["eth_chainId", "eth_getBalance"]);
        let err =
            validate_query_methods(&["eth_sendRawTransaction".to_string()]).expect_err("send");
        assert!(err.to_string().contains("not a read-only"));
    }

    #[test]
    fn debug_and_sign_typed_data_are_forbidden() {
        for method in [
            "debug_traceTransaction",
            "trace_block",
            "eth_signTypedData_v4",
            "eth_accounts",
            "eth_subscribe",
        ] {
            let err = validate_query_methods(&[method.to_string()]).expect_err(method);
            assert!(
                err.to_string().contains("not a read-only"),
                "{method}: {err}"
            );
        }
    }

    #[test]
    fn unknown_read_is_rejected() {
        let err = validate_query_methods(&["eth_getProof".to_string()]).expect_err("proof");
        assert!(err.to_string().contains("ceiling"));
    }

    #[test]
    fn duplicates_and_whitespace_normalize() {
        let allowed = validate_query_methods(&[
            " eth_chainId ".to_string(),
            "eth_chainId".to_string(),
            "eth_blockNumber".to_string(),
        ])
        .expect("ok");
        assert_eq!(allowed, vec!["eth_chainId", "eth_blockNumber"]);
    }
}
