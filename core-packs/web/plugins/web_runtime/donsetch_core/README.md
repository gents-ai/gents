# donsetch_core

This crate packages the portable part of DonSeTch 4.10.1 for a self-contained Gents WASM pack. It is derived from upstream revision `0e22d6784ede8764c5f20dce52956225654b21de` (`https://github.com/dondai44423/donsetch`) and is distributed under the upstream AGPL-3.0-only license in [license.txt](license.txt).

The crate retains upstream HTML extraction, charset handling, feeds, SERP parsers, query operators, intent classification, consensus/lexical ranking, wall detection, and sitemap/robots parsing. It adapts those modules to a library without native configuration, disk state, async runtime, HTTP transport, browser, or ONNX runtime. Domain-specific HTML/JSON adapters (GitHub, Stack Exchange, Wikipedia infoboxes, docs frameworks, Reddit JSON/package APIs) are omitted; the portable HTML pipeline and dedicated Reddit/Hacker News HTML extractors remain.

PDF text extraction uses the pure-Rust `pdf-extract` crate over in-memory bytes and sends extracted text through the shared HTML block pipeline. Image-only/scanned documents return an explicit extraction error; OCR, PDF page metadata, and layout reconstruction are outside this portable core.

The files under `src/` are vendored/adapted upstream source. Changes include removing native service integrations, removing debug-file writes, restoring SHA-256 fingerprint semantics locally, providing host-neutral robots/sitemap parsers, and adding pure-Rust text PDF extraction. Upstream modules retain their original copyright and attribution notices where present.

Build and test from this directory:

```sh
cargo test
cargo check --target wasm32-wasip1
```
