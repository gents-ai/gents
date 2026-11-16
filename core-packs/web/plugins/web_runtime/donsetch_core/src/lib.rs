//! Portable DonSeTch core extracted from upstream DonSeTch 4.10.1.
//!
//! Upstream: <https://github.com/dondai44423/donsetch>, revision
//! `0e22d6784ede8764c5f20dce52956225654b21de`.
//! This crate is adapted for host-driven WASM: native HTTP, Tokio,
//! Chromium, ONNX, config/state, and PDFium integrations are excluded.
//! HTML extraction, SERP parsing, query compilation, ranking, intent,
//! robots rules, and sitemap parsing remain derived from upstream.

pub mod crawl;
pub mod detect;
pub mod extract;
pub mod search;

#[cfg(test)]
mod portable_tests;
