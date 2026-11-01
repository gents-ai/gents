//! Build identity of the Claude Code release this crate reproduces.
//!
//! Every value is read from the extracted Claude Code 2.1.311 client (git
//! `e7d3b836`, Bun 1.4.4). Adopting another release means re-extracting it and
//! updating these constants and the rules that cite its chunks together.

/// Build-info `VERSION` (`chunk-1twhepc1.js`).
pub const VERSION: &str = "2.1.311";

/// Build-info `GIT_SHA`.
pub const GIT_SHA: &str = "e7d3b8365a0ffbe2f5dd032feb28e9fd9c7de025";

/// The vendored Anthropic SDK version sent as `X-Stainless-Package-Version`
/// (`chunk-d4tnenj8.js` `RR`).
pub const SDK_VERSION: &str = "0.128.0";

/// `process.version` under Bun 1.4.4, sent as `X-Stainless-Runtime-Version`.
pub const RUNTIME_VERSION: &str = "v26.12.1";

/// `anthropic-version`.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// First-party API origin.
pub const BASE_URL: &str = "https://api.anthropic.com";

/// Every Messages call goes through the SDK's beta namespace, for API keys too.
pub const MESSAGES_PATH: &str = "/v1/messages?beta=true";

/// Client request timeout (`F2r`), sent as `X-Stainless-Timeout: 600`.
pub const REQUEST_TIMEOUT_MS: u64 = 600_000;
