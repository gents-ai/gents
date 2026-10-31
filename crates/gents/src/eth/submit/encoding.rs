use alloy_primitives::{keccak256, Address, U256};
use anyhow::{anyhow, bail, Context, Result};

use super::super::keys::sign_prehash_recoverable;

pub(super) fn raw_transaction_hash(raw_transaction: &str) -> Result<String> {
    let bytes = decode_hex(raw_transaction)?;
    Ok(format!("{:#x}", keccak256(bytes)))
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

pub(super) fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}
