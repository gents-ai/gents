//! Chain-key material and Ethereum address derivation.
//!
//! Private keys never enter agent context. Documents hold the address and a
//! DID attestation only; the secret lives behind [`ChainKeyMaterialStore`].

#[cfg(test)]
use rapidhash::RapidHashMap;
#[cfg(test)]
use std::sync::{Arc, Mutex};

#[cfg(test)]
use anyhow::anyhow;
use anyhow::{bail, Context, Result};
use k256::ecdsa::SigningKey;
use k256::elliptic_curve::rand_core::OsRng;
use k256::elliptic_curve::zeroize::Zeroizing;
use sha3::{Digest, Keccak256};

pub const KEYRING_SERVICE: &str = "gents-chain-key";
pub const KEY_BACKEND_KEYRING: &str = "keyring";
const UNCOMPRESSED_PREFIX: u8 = 0x04;

/// Store for 32-byte secp256k1 secrets, keyed by an opaque custody key.
pub trait ChainKeyMaterialStore: Send + Sync {
    fn store_new(&self, storage_key: &str, secret: &[u8; 32]) -> Result<()>;
    fn load(&self, binding_id: &str) -> Result<[u8; 32]>;
    fn delete(&self, binding_id: &str) -> Result<()>;
}

/// Namespace OS-keyring accounts by principal so separate gents homes cannot
/// overwrite each other's identically named bindings.
pub fn binding_storage_key(principal_did: &str, binding_id: &str) -> String {
    format!("{}:{}", principal_did.trim(), binding_id.trim())
}

#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct MemoryChainKeyStore {
    inner: Arc<Mutex<RapidHashMap<String, [u8; 32]>>>,
}

#[cfg(test)]
impl ChainKeyMaterialStore for MemoryChainKeyStore {
    fn store_new(&self, storage_key: &str, secret: &[u8; 32]) -> Result<()> {
        let mut entries = self.inner.lock().expect("chain key store lock poisoned");
        if entries.contains_key(storage_key) {
            bail!("chain key material already exists for {storage_key}");
        }
        entries.insert(storage_key.to_string(), *secret);
        Ok(())
    }

    fn load(&self, binding_id: &str) -> Result<[u8; 32]> {
        self.inner
            .lock()
            .expect("chain key store lock poisoned")
            .get(binding_id)
            .copied()
            .ok_or_else(|| anyhow!("no chain key material for binding {binding_id}"))
    }

    fn delete(&self, binding_id: &str) -> Result<()> {
        self.inner
            .lock()
            .expect("chain key store lock poisoned")
            .remove(binding_id);
        Ok(())
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct KeyringChainKeyStore;

impl ChainKeyMaterialStore for KeyringChainKeyStore {
    fn store_new(&self, storage_key: &str, secret: &[u8; 32]) -> Result<()> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, storage_key)
            .context("opening OS keyring entry")?;
        match entry.get_password() {
            Ok(_) => bail!("chain key material already exists for {storage_key}"),
            Err(keyring::Error::NoEntry) => {}
            Err(error) => return Err(error).context("checking OS keyring for an existing key"),
        }
        let encoded = Zeroizing::new(lowercase_hex(secret));
        entry
            .set_password(&encoded)
            .context("storing chain key in OS keyring")?;
        Ok(())
    }

    fn load(&self, binding_id: &str) -> Result<[u8; 32]> {
        let entry =
            keyring::Entry::new(KEYRING_SERVICE, binding_id).context("opening OS keyring entry")?;
        let hex = Zeroizing::new(
            entry
                .get_password()
                .context("loading chain key from OS keyring")?,
        );
        parse_secret_hex(&hex)
            .with_context(|| format!("decoding chain key for binding {binding_id}"))
    }

    fn delete(&self, binding_id: &str) -> Result<()> {
        let entry =
            keyring::Entry::new(KEYRING_SERVICE, binding_id).context("opening OS keyring entry")?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error).context("deleting chain key from OS keyring"),
        }
    }
}

pub fn generate_secp256k1_secret() -> [u8; 32] {
    let signing_key = SigningKey::random(&mut OsRng);
    signing_key.to_bytes().into()
}

pub fn uncompressed_pubkey(secret: &[u8; 32]) -> Result<[u8; 65]> {
    let signing_key = SigningKey::from_bytes(secret.into()).context("loading secp256k1 secret")?;
    let point = signing_key.verifying_key().to_encoded_point(false);
    let bytes = point.as_bytes();
    if bytes.len() != 65 {
        bail!(
            "uncompressed secp256k1 public key must be 65 bytes, got {}",
            bytes.len()
        );
    }
    let mut out = [0u8; 65];
    out.copy_from_slice(bytes);
    Ok(out)
}

pub fn address_from_uncompressed_pubkey(uncompressed: &[u8]) -> Result<String> {
    let body = match uncompressed {
        [UNCOMPRESSED_PREFIX, rest @ ..] if rest.len() == 64 => rest,
        rest if rest.len() == 64 => rest,
        other => bail!(
            "expected 64-byte uncompressed secp256k1 public key, got {} bytes",
            other.len()
        ),
    };
    let hash = Keccak256::digest(body);
    Ok(format!("0x{}", lowercase_hex(&hash[12..])))
}

pub fn address_from_secret(secret: &[u8; 32]) -> Result<String> {
    address_from_uncompressed_pubkey(&uncompressed_pubkey(secret)?)
}

/// Canonical attestation payload signed by the principal DID.
pub fn attestation_payload(
    binding_id: &str,
    principal_did: &str,
    address: &str,
    key_backend: &str,
    created_at: &str,
) -> Vec<u8> {
    format!(
        "gents-chain-key-attestation-v1\n{binding_id}\n{principal_did}\n{address}\n{key_backend}\n{created_at}"
    )
    .into_bytes()
}

pub fn encode_attestation(signature: &[u8]) -> String {
    format!("0x{}", lowercase_hex(signature))
}

pub(crate) fn decode_attestation(value: &str) -> Result<Vec<u8>> {
    let hex = value.strip_prefix("0x").unwrap_or(value).trim();
    if !hex.len().is_multiple_of(2) {
        bail!("chain key attestation has odd-length hex");
    }
    (0..hex.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&hex[index..index + 2], 16)
                .context("chain key attestation is invalid hex")
        })
        .collect()
}

/// Recoverable secp256k1 signature over a 32-byte digest (Ethereum prehash).
pub fn sign_prehash_recoverable(
    secret: &[u8; 32],
    hash: &[u8; 32],
) -> Result<([u8; 32], [u8; 32], bool)> {
    let signing_key = SigningKey::from_bytes(secret.into()).context("loading secp256k1 secret")?;
    let (signature, recid) = signing_key
        .sign_prehash_recoverable(hash)
        .context("signing ethereum digest")?;
    let bytes = signature.to_bytes();
    let mut r = [0u8; 32];
    let mut s = [0u8; 32];
    r.copy_from_slice(&bytes[..32]);
    s.copy_from_slice(&bytes[32..]);
    Ok((r, s, recid.to_byte() & 1 == 1))
}

fn parse_secret_hex(value: &str) -> Result<[u8; 32]> {
    let hex = value.strip_prefix("0x").unwrap_or(value).trim();
    if hex.len() != 64 {
        bail!(
            "chain key secret must be 32 bytes hex, got {} chars",
            hex.len()
        );
    }
    let mut secret = [0u8; 32];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        let text = std::str::from_utf8(chunk).context("chain key hex is not utf8")?;
        secret[i] = u8::from_str_radix(text, 16).context("chain key hex is invalid")?;
    }
    Ok(secret)
}

fn lowercase_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Anvil / Foundry default account #0.
    const ANVIL0_SECRET: [u8; 32] = [
        0xac, 0x09, 0x74, 0xbe, 0xc3, 0x9a, 0x17, 0xe3, 0x6b, 0xa4, 0xa6, 0xb4, 0xd2, 0x38, 0xff,
        0x94, 0x4b, 0xac, 0xb4, 0x78, 0xcb, 0xed, 0x5e, 0xfc, 0xae, 0x78, 0x4d, 0x7b, 0xf4, 0xf2,
        0xff, 0x80,
    ];

    #[test]
    fn keyring_store_round_trips_across_entry_handles() {
        let store = KeyringChainKeyStore;
        let storage_key = format!(
            "gents-keyring-test-{}",
            lowercase_hex(&generate_secp256k1_secret()[..8])
        );
        struct Cleanup<'a>(&'a KeyringChainKeyStore, String);
        impl Drop for Cleanup<'_> {
            fn drop(&mut self) {
                let _ = self.0.delete(&self.1);
            }
        }
        let cleanup = Cleanup(&store, storage_key.clone());
        if let Err(error) = store.store_new(&storage_key, &ANVIL0_SECRET) {
            // Native backend compiled in, but this host has no usable OS
            // keyring (headless CI without keyutils, locked keychain).
            // The mock backend is still caught: it "stores" successfully
            // and then load fails below.
            eprintln!("skipping OS keyring round-trip: {error:#}");
            drop(cleanup);
            return;
        }
        let loaded = store.load(&storage_key).unwrap_or_else(|error| {
            panic!(
                "keyring store succeeded but load failed for {storage_key}: {error:#} \
                 (native keyring feature is probably not enabled)"
            )
        });
        assert_eq!(loaded, ANVIL0_SECRET);
        store.delete(&storage_key).expect("delete");
        assert!(store.load(&storage_key).is_err());
        drop(cleanup);
    }
}
