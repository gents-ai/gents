use std::num::{ParseIntError, TryFromIntError};
use std::str::Utf8Error;

use serde_json::Value;

use super::rpc::{ETH_GET_LOGS_MAX_RANGE, ETH_USER_AGENT};

type AddressParseError = <alloy_primitives::Address as std::str::FromStr>::Err;
type UintParseError = <alloy_primitives::U256 as std::str::FromStr>::Err;
type IntParseError = <alloy_primitives::I256 as std::str::FromStr>::Err;

pub(crate) type Result<T, E = EthError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum EthError {
    #[error("chain key material already exists for {storage_key}")]
    KeyMaterialExists { storage_key: String },
    #[cfg(test)]
    #[error("no chain key material for binding {binding_id}")]
    NoKeyMaterial { binding_id: String },
    #[error("opening OS keyring entry")]
    KeyringOpen(#[source] keyring::Error),
    #[error("checking OS keyring for an existing key")]
    KeyringCheck(#[source] keyring::Error),
    #[error("storing chain key in OS keyring")]
    KeyringStore(#[source] keyring::Error),
    #[error("loading chain key from OS keyring")]
    KeyringLoad(#[source] keyring::Error),
    #[error("deleting chain key from OS keyring")]
    KeyringDelete(#[source] keyring::Error),
    #[error("decoding chain key for binding {binding_id}")]
    KeyDecode {
        binding_id: String,
        #[source]
        source: Box<EthError>,
    },
    #[error("loading secp256k1 secret")]
    Secp256k1Secret(#[source] k256::ecdsa::Error),
    #[error("signing ethereum digest")]
    SigningDigest(#[source] k256::ecdsa::Error),
    #[error("uncompressed secp256k1 public key must be 65 bytes, got {len}")]
    UncompressedPubkeyLength { len: usize },
    #[error("expected 64-byte uncompressed secp256k1 public key, got {len} bytes")]
    PubkeyLength { len: usize },
    #[error("chain key attestation has odd-length hex")]
    AttestationOddHex,
    #[error("chain key attestation is invalid hex")]
    AttestationHex(#[source] ParseIntError),
    #[error("chain key secret must be 32 bytes hex, got {chars} chars")]
    SecretHexLength { chars: usize },
    #[error("chain key hex is not utf8")]
    SecretHexUtf8(#[source] Utf8Error),
    #[error("chain key hex is invalid")]
    SecretHex(#[source] ParseIntError),

    #[error("eth rpc_timeout_secs must be positive")]
    ZeroTimeout,
    #[error("building eth JSON-RPC HTTP client")]
    HttpClient(#[source] reqwest::Error),
    #[error("posting eth JSON-RPC to {url}")]
    Post {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("reading eth JSON-RPC body from {url}")]
    ReadBody {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("eth JSON-RPC {url} returned 403; public endpoints (e.g. Base) require User-Agent {ETH_USER_AGENT}")]
    HttpForbidden { url: String },
    #[error("eth JSON-RPC {url} failed with {status}: {text}")]
    HttpStatus {
        url: String,
        status: reqwest::StatusCode,
        text: String,
    },
    #[error("decoding eth JSON-RPC body from {url}: {text}")]
    DecodeBody {
        url: String,
        text: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("eth RPC URL is empty")]
    RpcUrlEmpty,
    #[error("eth chain_id must be a positive integer")]
    ChainIdZero,
    #[error("eth method {method} is not in the configured query_methods ∩ builtin ceiling")]
    MethodNotConfigured { method: String },
    #[error("latest block missing baseFeePerGas: {block}")]
    BaseFeeMissing { block: Value },
    #[error("baseFeePerGas is not hex: {fee}")]
    BaseFeeNotHex { fee: String },
    #[error("decoding baseFeePerGas {fee}")]
    BaseFeeDecode {
        fee: String,
        #[source]
        source: ParseIntError,
    },
    #[error("eth_getBlockByNumber latest")]
    LatestBlock {
        #[source]
        source: Box<EthError>,
    },
    #[error("eth chain_id mismatch: document {expected}, endpoint {actual:#x}")]
    ChainIdCached { expected: u64, actual: u64 },
    #[error("eth chain_id mismatch: document {expected}, endpoint {actual} ({actual:#x})")]
    ChainIdMismatch { expected: u64, actual: u64 },
    #[error("decoding eth_chainId")]
    DecodeChainId {
        #[source]
        source: Box<EthError>,
    },
    #[error("eth JSON-RPC params must be an array, got {got}")]
    RpcParamsNotArray { got: Value },
    #[error("eth pruned-history error (not retried): {message}")]
    PrunedHistory { message: String },
    #[error("eth call reverted: {reason}")]
    Reverted { reason: String },
    #[error("eth JSON-RPC error {code}: {message}")]
    RpcError { code: Value, message: String },
    #[error("eth JSON-RPC response missing result: {response}")]
    MissingResult { response: Value },
    #[error("eth_getLogs requires a filter object as params[0]")]
    GetLogsFilterMissing,
    #[error("eth_getLogs requires address and/or topics; unfiltered log queries are rejected")]
    GetLogsUnfiltered,
    #[error("eth_getLogs blockHash cannot be combined with fromBlock/toBlock")]
    GetLogsBlockHashWithRange,
    #[error("eth_getLogs blockHash must be a 32-byte hex string")]
    GetLogsBlockHashInvalid,
    #[error("eth_getLogs fromBlock/toBlock earliest is unbounded and rejected")]
    GetLogsUnboundedEarliest,
    #[error("eth_getLogs toBlock is before fromBlock")]
    GetLogsReversedRange,
    #[error("eth_getLogs range {blocks} exceeds max {ETH_GET_LOGS_MAX_RANGE} blocks")]
    GetLogsRangeTooLarge { blocks: u64 },
    #[error("eth_getLogs numeric-to-dynamic or mixed dynamic ranges are unbounded and rejected")]
    GetLogsMixedRange,
    #[error("eth block number {number} is not a u64")]
    BlockNumberNotU64 { number: serde_json::Number },
    #[error("eth block tag must be a string or number, got {got}")]
    BlockTagType { got: Value },
    #[error("expected hex quantity, got {got}")]
    QuantityType { got: Value },
    #[error("parsing hex quantity {value}")]
    HexQuantity {
        value: String,
        #[source]
        source: ParseIntError,
    },
    #[error("parsing quantity {value}")]
    Quantity {
        value: String,
        #[source]
        source: ParseIntError,
    },
    #[cfg(test)]
    #[error("scripted eth RPC has no remaining responses")]
    ScriptExhausted,

    #[error("query method {method} is not in the builtin read-only ceiling")]
    QueryMethodNotCeiling { method: String },
    #[error("query method {method} is not a read-only JSON-RPC method")]
    QueryMethodForbidden { method: String },
    #[error("EthTool {tool_id} has an empty rpc_url")]
    EthToolEmptyRpcUrl { tool_id: String },
    #[error("EthTool {tool_id} has no chain_id")]
    EthToolNoChainId { tool_id: String },
    #[error("EthTool {tool_id} chain_id must be positive")]
    EthToolChainIdNotPositive { tool_id: String },
    #[error("signing call declarations require key_binding_id")]
    SigningRequiresKeyBinding,
    #[error("EthTool.calls: {error}")]
    CallsDecode { error: String },
    #[error("EthTool.calls contains any_read more than once")]
    AnyReadRepeated,
    #[error("sign_typed_data.primary_type is empty")]
    SignTypedPrimaryEmpty,
    #[error("sign_typed_data.domain must be an object")]
    SignTypedDomainNotObject,
    #[error("sign_typed_data.types must be an object")]
    SignTypedTypesNotObject,
    #[error("sign_typed_data {tool_name} domain chainId {domain_chain_id} does not match EthTool chain_id {chain_id}")]
    SignTypedDomainMismatch {
        tool_name: String,
        domain_chain_id: u64,
        chain_id: u64,
    },
    #[error("sign_typed_data {tool_name} domain requires chainId matching EthTool chain_id")]
    SignTypedDomainMissing { tool_name: String },
    #[error("EIP-712 domain chainId is not a positive integer")]
    Eip712ChainIdNotPositive,
    #[error("invalid EIP-712 domain chainId {text:?}: {error}")]
    Eip712ChainIdParse { text: String, error: ParseIntError },
    #[error("EIP-712 domain chainId must be a string or integer, got {got}")]
    Eip712ChainIdType { got: Value },
    #[error("write tool {tool_name} requires a positive max_gas")]
    WriteRequiresMaxGas { tool_name: String },
    #[error("write tool {tool_name} requires max_fee_per_gas")]
    WriteRequiresMaxFee { tool_name: String },
    #[error("write tool {tool_name} max_fee_per_gas")]
    WriteMaxFeeParse {
        tool_name: String,
        #[source]
        source: ParseIntError,
    },
    #[error("write tool {tool_name} max_fee_per_gas must be positive")]
    WriteMaxFeeZero { tool_name: String },
    #[error("ABI {abi} has {inputs} inputs but {params} parameter declarations")]
    ParamCountMismatch {
        abi: String,
        inputs: usize,
        params: usize,
    },
    #[error(
        "declared call ABI input {ty} is unnamed; use a named signature such as address recipient"
    )]
    UnnamedAbiInput { ty: String },
    #[error("declared call ABI has duplicate input name {name:?}")]
    DuplicateAbiInput { name: String },
    #[error("ABI input {name:?} has no matching parameter declaration")]
    AbiInputUndeclared { name: String },
    #[error("parameter declaration {extra:?} is not an ABI input")]
    ParamUndeclaredInAbi { extra: String },
    #[error("call param {name} has unsupported source {kind:?}")]
    ParamSourceUnsupported { name: String, kind: String },
    #[error("call param {name} source=fixed requires value")]
    ParamFixedNeedsValue { name: String },
    #[error("call param {name} runtime source must be self_address")]
    ParamRuntimeSource { name: String },
    #[error("call param {name} uses unsupported Solidity type {solidity_type}")]
    ParamUnsupportedType { name: String, solidity_type: String },
    #[error("call param {name} runtime self_address requires address type")]
    ParamRuntimeNeedsAddress { name: String },
    #[error("call param {name} has address_allowlist but type is {solidity_type}")]
    ParamAllowlistNotAddress { name: String, solidity_type: String },
    #[error("call param {name} address_allowlist is empty")]
    ParamAllowlistEmpty { name: String },
    #[error("call param {name} has min/max but type is {solidity_type}")]
    ParamMinMaxNotInteger { name: String, solidity_type: String },
    #[error("call param {name} enum is empty")]
    ParamEnumEmpty { name: String },
    #[error("write param {name} address has no allowlist (deny-by-default)")]
    WriteAddressNoAllowlist { name: String },
    #[error("write param {name} integer has no max (deny-by-default)")]
    WriteIntegerNoMax { name: String },
    #[error("write param {name} {solidity_type} has no enum (deny-by-default)")]
    WriteDataNoEnum { name: String, solidity_type: String },
    #[error("call param {name} min exceeds max")]
    ParamMinExceedsMax { name: String },
    #[error("invalid unsigned integer bound {value:?}")]
    UintBound {
        value: String,
        #[source]
        source: UintParseError,
    },
    #[error("invalid signed integer bound {value:?}")]
    IntBound {
        value: String,
        #[source]
        source: IntParseError,
    },
    #[error("call tool_name is empty")]
    ToolNameEmpty,
    #[error("duplicate call tool_name {name}")]
    DuplicateToolName { name: String },
    #[error("{field} must be a 20-byte 0x-address, got {value:?}")]
    AddressFormat { field: String, value: String },
    #[error("call signature is empty")]
    SignatureEmpty,
    #[error("invalid ABI signature {signature:?}: {error}")]
    AbiSignatureInvalid {
        signature: String,
        error: alloy_json_abi::parser::Error,
    },
    #[error("sign_typed_data.types has no {primary_type:?} field list")]
    TypedTypesNoFieldList { primary_type: String },
    #[error("EIP-712 {primary_type} has {fields} fields but {params} parameter declarations")]
    TypedFieldCount {
        primary_type: String,
        fields: usize,
        params: usize,
    },
    #[error("EIP-712 {primary_type} field has no name")]
    TypedFieldNoName { primary_type: String },
    #[error("EIP-712 {primary_type}.{name} has no type")]
    TypedFieldNoType { primary_type: String, name: String },
    #[error("EIP-712 {primary_type} has duplicate field {name:?}")]
    TypedFieldDuplicate { primary_type: String, name: String },
    #[error("EIP-712 field {name:?} has no parameter declaration")]
    TypedFieldUndeclared { name: String },

    #[error("sign_typed_data {tool_name} requires a chain key binding")]
    SignTypedRequiresBinding { tool_name: String },
    #[error("write tool {tool_name} requires a chain key binding")]
    WriteRequiresBinding { tool_name: String },
    #[error("native_transfer {tool_name} requires a chain key binding")]
    NativeTransferRequiresBinding { tool_name: String },
    #[error(
        "sign_typed_data {tool_name} domain chainId does not match EthTool chain_id {chain_id}"
    )]
    SignTypedChainMismatch { tool_name: String, chain_id: u64 },
    #[error(
        "sign_typed_data {tool_name} domain requires chainId matching EthTool chain_id {chain_id}"
    )]
    SignTypedChainMissing { tool_name: String, chain_id: u64 },
    #[error("undeclared model param {extra}")]
    UndeclaredModelParam { extra: String },
    #[error("missing model param {name}")]
    MissingModelParam { name: String },
    #[error("runtime self_address is unavailable")]
    SelfAddressUnavailable,
    #[error("unsupported param source {kind}")]
    UnsupportedParamSource { kind: String },
    #[error("param {name} address must be a string")]
    ParamAddressNotString { name: String },
    #[error("param {name} address {address} is not on the allowlist")]
    ParamAddressNotAllowed { name: String, address: String },
    #[error("param {name} value {value} is not in the enum")]
    ParamNotInEnum { name: String, value: String },
    #[error("param {name} is below min {min}")]
    ParamBelowMin { name: String, min: String },
    #[error("param {name} exceeds max {max}")]
    ParamExceedsMax { name: String, max: String },
    #[error("enum constraint does not support value {value}")]
    EnumConstraintValue { value: Value },
    #[error("max_fee_per_gas {text}: {error}")]
    MaxFeeParse { text: String, error: ParseIntError },
    #[error("signature {signature} expects {expected} args, got {got}")]
    SignatureArgCount {
        signature: String,
        expected: usize,
        got: usize,
    },
    #[error("encoding argument {name} as {ty}")]
    EncodeArgument {
        name: String,
        ty: String,
        #[source]
        source: Box<EthError>,
    },
    #[error("ABI encode {signature}: {error}")]
    AbiEncode {
        signature: String,
        error: alloy_dyn_abi::Error,
    },
    #[error("address must be a string")]
    AddressNotString,
    #[error("address {text}: {error}")]
    AddressParse {
        text: String,
        error: AddressParseError,
    },
    #[error("bool expected")]
    BoolExpected,
    #[error("string expected")]
    StringExpected,
    #[error("invalid fixed bytes type {ty}")]
    FixedBytesType {
        ty: String,
        #[source]
        source: ParseIntError,
    },
    #[error("fixed bytes size must be between 1 and 32, got {size}")]
    FixedBytesSize { size: usize },
    #[error("{ty} must be a hex string")]
    HexStringExpected { ty: String },
    #[error("{ty} requires exactly {size} bytes, got {got}")]
    FixedBytesLength { ty: String, size: usize, got: usize },
    #[error("bytes must be a hex string")]
    BytesHexExpected,
    #[error("{ty} value {value} exceeds {bits} bits")]
    IntegerOutOfRange {
        ty: String,
        value: String,
        bits: usize,
    },
    #[error("unsupported Solidity type {ty}")]
    UnsupportedSolidityType { ty: String },
    #[error("uint must not be a float: {n}")]
    UintFloat { n: serde_json::Number },
    #[error("uint {n} is not an integer")]
    UintNotInteger { n: serde_json::Number },
    #[error("uint expected string or integer, got {got}")]
    UintExpected { got: Value },
    #[error("uint {text}: {error}")]
    UintParse { text: String, error: UintParseError },
    #[error("int {text}: {error}")]
    IntParse { text: String, error: IntParseError },
    #[error("int {n} is not an integer")]
    IntNotInteger { n: serde_json::Number },
    #[error("int expected string or integer, got {got}")]
    IntExpected { got: Value },
    #[error("odd-length hex {value}")]
    OddHex { value: String },
    #[error("invalid hex: {error}")]
    InvalidHex { error: ParseIntError },
    #[error(transparent)]
    HexByte(ParseIntError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[cfg(test)]
    #[error("send has no raw transaction")]
    ScriptedSendMissingRaw,

    #[error("eth submit requires an idempotency key")]
    SubmitIdempotencyKeyMissing,
    #[error("eth submit requires principal_did")]
    SubmitPrincipalMissing,
    #[error("eth submit chain_id must be positive")]
    SubmitChainIdZero,
    #[error("invalid submit from address")]
    SubmitFromInvalid {
        #[source]
        source: AddressParseError,
    },
    #[error("eth submit from address must not be zero")]
    SubmitFromZero,
    #[error("invalid submit to address")]
    SubmitToInvalid {
        #[source]
        source: AddressParseError,
    },
    #[error("chain_id exceeds DefraDB Int")]
    ChainIdExceedsInt {
        #[source]
        source: TryFromIntError,
    },
    #[error("nonce exceeds DefraDB Int")]
    NonceExceedsInt {
        #[source]
        source: TryFromIntError,
    },
    #[error("signing key address {derived} does not match from address {from}")]
    SignerMismatch { derived: String, from: String },
    #[error("deriving submit signer address")]
    SubmitSignerAddress {
        #[source]
        source: Box<EthError>,
    },
    #[error(
        "idempotency key {idempotency_key:?} was already used for a different Ethereum request"
    )]
    IdempotencyKeyReused { idempotency_key: String },
    #[error("persisted Ethereum submission identity does not match the request")]
    PersistedIdentityMismatch,
    #[error("persisted Ethereum submission raw transaction hash is inconsistent")]
    PersistedHashInconsistent,
    #[error("persisted Ethereum submission has a negative nonce")]
    PersistedNegativeNonce,
    #[error("decoding persisted Ethereum receipt")]
    DecodePersistedReceipt {
        #[source]
        source: serde_json::Error,
    },
    #[error("eth_getTransactionReceipt")]
    ReceiptFetch {
        #[source]
        source: Box<EthError>,
    },
    #[error("simulating eth transaction")]
    SimulateTransaction {
        #[source]
        source: Box<EthError>,
    },
    #[error("simulation reverted: {reason}")]
    SimulationReverted { reason: String },
    #[error("eth_getTransactionCount")]
    TransactionCountFetch {
        #[source]
        source: Box<EthError>,
    },
    #[error("eth_getTransactionCount returned non-string {result}")]
    TransactionCountNotString { result: Value },
    #[error("decoding nonce")]
    DecodeNonce {
        #[source]
        source: Box<EthError>,
    },
    #[error("eth_estimateGas")]
    EstimateGasFetch {
        #[source]
        source: Box<EthError>,
    },
    #[error("eth_estimateGas returned non-string {estimate}")]
    EstimateGasNotString { estimate: Value },
    #[error("decoding gas estimate")]
    DecodeGasEstimate {
        #[source]
        source: Box<EthError>,
    },
    #[error("estimated gas {gas_limit} exceeds max_gas {cap}")]
    GasLimitExceedsCap { gas_limit: u64, cap: u64 },
    #[error("eth_maxPriorityFeePerGas")]
    PriorityFeeFetch {
        #[source]
        source: Box<EthError>,
    },
    #[error("eth_getBlockByNumber latest baseFeePerGas")]
    BaseFeeFetch {
        #[source]
        source: Box<EthError>,
    },
    #[error("max fee per gas overflow")]
    MaxFeeOverflow,
    #[error("max fee per gas {max_fee} exceeds cap {cap}")]
    MaxFeeExceedsCap { max_fee: u128, cap: u128 },
    #[error("{method} returned non-string {value}")]
    RpcU128NotString { method: String, value: Value },
    #[error("{method} returned non-hex quantity {text:?}")]
    RpcU128NotHex { method: String, text: String },
    #[error("decoding {method} result {text}")]
    RpcU128Decode {
        method: String,
        text: String,
        #[source]
        source: ParseIntError,
    },
    #[error("parsing tx to")]
    ParseTo {
        #[source]
        source: AddressParseError,
    },
}
