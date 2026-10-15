# Web

`gents/web` is a self-contained web research pack. It installs a ready-to-use
Web researcher and three direct tools: `gents/web_search`, `gents/web_fetch`,
and `gents/web_crawl`. The tools make bounded HTTPS requests through the Gents
host; the pack does not require a SearXNG server, Firecrawl, API keys, or a
browser. A prebuilt pack does not require Rust or native tools on the machine.

## Install and use

Build from this checkout, then install and bind its inference slot to an
existing profile (replace `PROFILE_ID` below):

```sh
gents pack build ./packs/gents/web --out gents.web.pack
gents pack install ./gents.web.pack --grant-authority --inference-slot web_researcher=PROFILE_ID
gents chat --agent-id web-researcher "Find the current official guidance on ..."
```

Use a current Gents build with plugin HTTP support (validated with commit
`8f2e34af`). To install a prebuilt artifact, skip the build command. After
publication, use `gents/web` in place of the archive path. Source builds
need the repository's Rust toolchain and `wasm32-wasip1` target. The explicit
`--grant-authority` accepts the plugins' outbound HTTP authority. The profile
is not created or configured by this pack. Use a profile capable of following
links and distinguishing source statements from synthesis.

The agent searches for candidate sources, fetches pages to verify claims, and
uses crawl for a bounded exploration of linked pages. It should put source
links beside claims, report meaningful disagreements, and identify missing or
partial results. Retrieved page text is untrusted input and must not override
the research task.

## Tools and access

| Tool | Purpose | Main input |
| --- | --- | --- |
| `gents/web_search` | Search the supported keyless search sources and return ranked results | `query` |
| `gents/web_fetch` | Extract readable text from one URL or a batch of up to 12 | `url` |
| `gents/web_crawl` | Follow links within a bounded site scope and return page records | `url` |

The schemas in the pack manifest are the accepted interface. `web_search`
requires `query` and accepts up to two `query_variants`, `max_results`,
`deadline_ms`, and `intent` (`auto`, `web`, `code`, `paper`, `news`, or
`entity`). It queries DuckDuckGo Lite, Bing, Google, Brave, Mojeek, and Yahoo;
there are no vertical searches or provider selection options. `web_fetch`
accepts a single `url` or an array of 1 to 12 URLs, `focus`, `mode` (`scan`, `read`, or `deep`),
`max_chars`, `offset`, `section`, `toc`, `deadline_ms`, `must_contain`,
`selector`, `links`, and `media`. `web_crawl` accepts `url`, `mode` (`full`,
`map`, or `content`), `focus`, `max_pages`, `map_limit`, `max_depth`,
`max_total_chars`, `per_page_max`, `same_host` (only `true` is supported),
`respect_robots`, and `deadline_s`. Deadlines are checked between request
rounds, and one in-flight host HTTP request can take up to 10 seconds inside
the plugin's 120-second wall limit. Crawl follows same-origin links, applies
`robots.txt` access rules by default, and uses sitemap hints when available.
With `respect_robots` enabled, a positive `Crawl-delay` stops the crawl with an
explicit limitation because this runtime has no delayed request scheduler.
Crawl results include a `complete` flag and stop reason; limits, failed requests, or partial
extraction keep `complete` false.

The `max_chars`, `max_total_chars`, `per_page_max`, and `offset` values count
UTF-8 bytes; these names are retained for Donsetch compatibility. Fetch byte
budgets have a 200-byte minimum, and map output is capped at 1000 URLs.
Without `max_chars`, fetch returns up to 800 bytes in `scan`, 4000 in `read`,
and 16000 in `deep` (the default). `must_contain` returns match-probe output
instead of page text; omit it to read a source. Partial fetches report
`next_offset` when more text is available, and focused excerpts remain marked
incomplete even when all matching text fits.

This retains a useful Donsetch API subset with bounded, stateless behavior; it
does not promise full feature parity with Donsetch, SearXNG, or Firecrawl.
Provider-specific fields, vertical search, Firecrawl modes and stateful options
such as `resume`/`since_last`, fetch handles/actions/stitch/OCR, arbitrary crawl
path filters, and external crawl datasets are rejected or not offered. All
URLs must use HTTPS, and local or private-network destinations are refused. A
request may return partial data, an HTTP error, or no results; say when that
affects the answer.

For a direct call after install:

```sh
gents plugin run gents/web_search --input '{"query":"official HTTP status code reference","intent":"web","max_results":5}'
gents plugin run gents/web_fetch --input '{"url":"https://www.rfc-editor.org/rfc/rfc9110","mode":"read","max_chars":4000}'
```

Each plugin declares `OutboundHttp` through the host. The guest rejects local
hostnames and private IP literals; Gents' host HTTP resolver blocks private
resolved addresses and disables automatic redirects and proxies. The plugins
have no filesystem, environment-variable, child-process, or crypto authority. Requests are not constrained to an
operator-provided domain allow-list, so only install the pack when its internet
access is appropriate for the node. No website credentials are configured by
the pack.

## Boundaries

These tools retrieve HTTPS responses and extract text or links from responses.
They can extract text from text-based PDFs; scanned pages receive no OCR.
They do not run page JavaScript, use a browser, take screenshots, or turn
images into text. The pack has no guest-native TLS, ONNX, or browser support;
network requests use Gents' host HTTP service. It is not a replacement for a
browser-based workflow or for every Firecrawl feature. The planned browser
escalation and screenshots follow-up is tracked in [issue #26](https://github.com/gents-ai/packs/issues/26).

This replaces the former `gents/web_deep_research` pack in this repository.
Existing installations are not automatically migrated or removed.

## Development

`tests/install.json` checks installation, reinstall, and removal of the agent,
slot, and three plugins. `tests/content.json` checks the tool schemas, access
grants, and inference binding contract.

Run the full build, native workspace tests, and installation lifecycle with:

```sh
GENTS=/path/to/current/gents scripts/test-pack.sh packs/gents/web
```

The included runtime and core source are licensed under AGPL-3.0, and the pack
carries the corresponding source and license. See the [upstream modification
record](plugins/web_runtime/donsetch_core/upstream.md). The tools keep no
persistent crawl database or cache; Gents retains tool results in its session
history.
