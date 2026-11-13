# `web_crawl`

Explore a bounded set of pages beginning at `url`. By default the crawl stays
on the starting origin and follows links found in fetched HTML. Use `max_pages`,
`max_depth`, and `max_total_chars` (UTF-8 bytes) to keep a crawl small; `mode` (`full`,
`map`, or `content`), `focus`, `map_limit`, and `per_page_max` to control the
returned crawl view. `respect_robots` defaults to true. `deadline_s` bounds
the crawl across request rounds; an in-flight host HTTP request can take up to
10 seconds, within the plugin's 120-second wall limit.
The `same_host` option is supported only as true; cross-origin traversal is
refused. With `respect_robots` enabled, a positive `Crawl-delay` stops the
crawl with an explicit limitation; delayed scheduling is not supported.

Crawl is best-effort HTML link traversal, not a JavaScript browser. It may
return partial pages, skip unreachable URLs, or stop at its limits. Stateful
resume, `since_last`, arbitrary path filters, and external dataset storage are
not supported.
