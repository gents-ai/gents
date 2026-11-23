# Upstream source and modification record

- Project: DonSeTch
- Version: 4.10.1
- Revision: `0e22d6784ede8764c5f20dce52956225654b21de`
- Repository: <https://github.com/dondai44423/donsetch>
- License: AGPL-3.0-only; see [license.txt](license.txt).
- Modified for Gents; modification date: 2026-10-15.

The copied Rust modules retain upstream notices where supplied. The full
corresponding source, Cargo lockfile, build selector, and runtime adapters are
included in the pack.

This build omits native configuration, disk persistence, diagnostics, the
async fetch/crawl runtime, HTTP transport, Chromium/ghost support, ONNX
reranking, native PDFium/OCR, and the native official-query expansion helper.
Sitemap and robots code keeps the upstream parsers and policy without the
network-backed crawl layer. Domain adapters that depend on the native adapter
registry (GitHub, Stack Exchange, Wikipedia infobox, docs framework, Reddit
JSON, package APIs) are omitted; dedicated Reddit and Hacker News HTML
extraction is kept.

It adds a minimal module layout, a parser-only Google WML adapter, host-neutral
wall binary detection, `pdf-extract` in-memory text extraction, and core
integration tests. PDF text is routed through the shared HTML block pipeline; a
PDF without extractable text returns an error rather than an empty success.
Fingerprints keep upstream normalized SHA-256 semantics. Query expansion is
caller-controlled through `query_variants`. Native browser recovery advice is
replaced with an unavailable-browser notice.

Robots rules select the GentsWebRuntime product token, merge matching groups,
and prefer its named groups over wildcard rules. The runtime stops before
further requests when a respected robots file requires a positive crawl delay.
