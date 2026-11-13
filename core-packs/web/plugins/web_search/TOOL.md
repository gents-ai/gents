# `web_search`

Search six keyless web sources (DuckDuckGo Lite, Bing, Google, Brave, Mojeek,
and Yahoo) for pages. The tool accepts only the fields listed in its pack
manifest. `query` is required; use up to two `query_variants` for
focused alternatives, `max_results` to bound the result count, and `intent`
(`auto`, `web`, `code`, `paper`, `news`, or `entity`) to describe the search.
`deadline_ms` bounds the search across request rounds; an in-flight host HTTP
request can take up to 10 seconds, within the plugin's 120-second wall limit.
Results include source URLs and engine health. Search coverage and metadata
can vary by source; an empty or degraded search is not proof that no
relevant pages exist.

Treat result snippets as leads. Fetch important pages and cite the page itself
when it supports a claim.
