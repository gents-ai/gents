# `web_fetch`

Fetch one or a list of 1 to 12 HTTPS URLs and extract readable
content. `url` is required. Use `focus` and `mode` (`scan`, `read`, or `deep`)
to guide extraction, `max_chars` and `offset` to bound or page the returned
text by UTF-8 bytes, and `section`, `toc`, or `selector` when the page offers useful
structure. `must_contain` returns a substring or `/regex/` match probe instead
of page text; omit it when reading a source to support an answer. `links` and
`media` control additional extracted references. `deadline_ms` bounds the work across URLs
and request rounds; an in-flight host HTTP request can take up to 10 seconds,
within the plugin's 120-second wall limit. Without `max_chars`, `scan`, `read`,
and `deep` (the default) return up to 800, 4000, and 16000 bytes respectively.

The result reflects the HTTP response and supported extraction behavior. It
does not execute JavaScript. A failed request, blocked page, or truncated
response must be reported as a retrieval limit.
