//! Native EVM surface: chain keys, JSON-RPC, and generated tools.

pub(crate) mod call_tool;
pub(crate) mod calls;
pub mod keys;
pub mod methods;
pub(crate) mod query;
pub(crate) mod rpc;

pub(crate) use call_tool::{EthCallTool, ResolvedEthCall};
pub(crate) use calls::{parse_call_decls, validate_call_decls};
pub use keys::{
    address_from_secret, address_from_uncompressed_pubkey, attestation_payload,
    binding_storage_key, encode_attestation, generate_secp256k1_secret, ChainKeyMaterialStore,
    KeyringChainKeyStore, KEYRING_SERVICE, KEY_BACKEND_KEYRING,
};
pub use methods::{
    method_permitted, normalize_method, validate_query_methods, BUILTIN_QUERY_METHODS,
};
pub(crate) use query::{EthQueryTool, ResolvedEthQuery};
pub use rpc::{HttpEthRpc, ETH_USER_AGENT};

pub fn validate_eth_call_declarations(calls: &[String]) -> anyhow::Result<()> {
    let declarations = parse_call_decls(Some(calls))?;
    validate_call_decls(&declarations)
}
