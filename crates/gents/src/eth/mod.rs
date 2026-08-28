//! Native EVM surface: chain keys, JSON-RPC, and generated tools.

pub mod keys;
pub mod methods;
pub(crate) mod rpc;

pub use keys::{
    address_from_secret, address_from_uncompressed_pubkey, attestation_payload,
    binding_storage_key, encode_attestation, generate_secp256k1_secret, ChainKeyMaterialStore,
    KeyringChainKeyStore, KEYRING_SERVICE, KEY_BACKEND_KEYRING,
};
pub use methods::{
    method_permitted, normalize_method, validate_query_methods, BUILTIN_QUERY_METHODS,
};
pub use rpc::{HttpEthRpc, ETH_USER_AGENT};
