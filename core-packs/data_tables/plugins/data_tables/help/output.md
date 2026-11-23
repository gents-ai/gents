```json
{"columns": [{"name": "region", "type": "text"}, {"name": "units", "type": "int64"}],
 "rows": [["north", 15], ["south", 10]], "row_count": 2, "offset": 0, "order": "columns",
 "markdown": "| region | units |\n| --- | --- |\n| north | 15 |\n| south | 10 |",
 "warnings": []}
```

`rows` is the page; `row_count` counts the rows in it and `offset` the rows before it. Types are `bool`,
`int8` to `int64`, `uint8` to `uint64`, `float32`, `float64`, `decimal(p,s)`, `text`, `binary` (lowercase hex),
`date`, `time`, `timestamp`, `timestamp_tz`, `list<...>`, `struct`, `map`. Values: integers are numbers, except
an `int64` or `uint64` beyond +-2^53, which is a string so no digit is lost; every decimal is a string; floats
are numbers in the shortest form that reads back identically, and NaN, Infinity and -Infinity are those strings;
NULL is `null` and the empty string is `""`; dates and timestamps are ISO-8601. A value over 64 KiB is cut and
`warnings` says so (a list or record cut this way is shown as the first 64 KiB of its JSON text); a row over 1 MiB
has its long texts shortened. `markdown` shows NULL as `NULL`. The whole result stays under 3.5 MB: a page that
would pass it holds fewer rows than `max_bytes` allows, says so in `warnings` and carries `next`; a result that
cannot fit even one row fails with a sentence saying to select fewer columns.

Row order: a query without ORDER BY still has one repeatable order: a plain scan (filters, projections and LIMIT
over one table) is in the file's row order, and anything else (GROUP BY, JOIN, DISTINCT, window functions, UNION)
is sorted by all its output columns, ascending, NULLs last. `order` says which rule applied: `query`, `file`,
`columns` or `none` (no sortable column, and more than one row). Rows that tie under your own ORDER BY come back in
the same order on every call.
