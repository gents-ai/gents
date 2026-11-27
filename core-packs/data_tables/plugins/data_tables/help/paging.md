A result longer than one page carries `response.next` (`{"cursor": "..."}`). Call again with the same fields and
`"cursor": "<that value>"`; the next page starts exactly where this one ended, nothing repeated or skipped,
and you may change `max_rows`. Repeat until `next` is absent. A cursor is refused, with the reason, when the
SQL, the options or the files changed. The query runs again for each page, so a very deep page of a large
sorted result costs a full run: narrow the query instead when you can.

`max_rows` (1 to 100000, default 200) and `max_bytes` (4096 to 3000000, default 1000000) bound one page of rows;
`markdown_rows` (0 to 1000, default 50) bounds the Markdown table, which stops at 256 KiB and says how many rows
it shows.
