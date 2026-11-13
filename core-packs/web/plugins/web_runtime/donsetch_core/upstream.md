# Upstream source and modification record

- Project: DonSeTch
- Version: 4.10.1
- Revision: `0e22d6784ede8764c5f20dce52956225654b21de`
- Repository: <https://github.com/dondai44423/donsetch>
- License: AGPL-3.0-only; see [license.txt](license.txt).

The copied Rust modules retain upstream notices where supplied. This pack removes native configuration, disk persistence, diagnostics, async fetch/crawl runtime, HTTP transport, Chromium/ghost support, ONNX reranking, and native PDFium/OCR. It removes the network-backed crawl layer from sitemap/robots code while retaining the upstream parsers and policy. It removes domain adapters that depend on the native adapter registry (GitHub, Stack Exchange, Wikipedia infobox, docs framework, Reddit JSON, package APIs). It keeps dedicated Reddit and Hacker News HTML extraction.

Portable changes add a minimal module layout, a parser-only Google WML adapter, host-neutral wall binary detection, `pdf-extract` in-memory text extraction, and core integration tests. PDF text is routed through the shared HTML block pipeline. PDFs without extractable text return an error; no empty-success result is fabricated. Fingerprints retain upstream normalized SHA-256 semantics.

Adapted for Gents on 2026-10-15. The full corresponding source, Cargo lockfile, build selector, and runtime adapters are included in the pack. Native browser recovery advice is replaced with an unavailable-browser notice.
The unused native official-query expansion helper and its test are removed;
query expansion remains caller-controlled through `query_variants`.

Robots rules select the GentsWebRuntime product token, merge matching groups,
and prefer its named groups over wildcard rules. The runtime stops before
further requests when a respected robots file requires a positive crawl delay.
