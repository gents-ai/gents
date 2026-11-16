# data_tables

Runs SQL over data files and returns exact results. Use it whenever the answer is in a CSV, TSV, JSON,
NDJSON, Parquet, XLSX or ODS file, or in rows another tool gave you: let SQL count, filter, group and join,
and read the result instead of the raw file. The files can be of any size; they are streamed, never loaded whole.

## Input

One JSON object. Name the data in one or both ways:

- `path`: a data file, or a folder of them (a relative path starts at the working folder). Only that file or
  folder is readable for the call. In a folder every readable file, below subfolders too, is a table; hidden
  files are ignored. `files` lists the relative paths inside the folder to use, in that order.
- `tables`: rows sent inline, in the shape this tool's own results have:
  `{"name": "t", "columns": [{"name": "a", "type": "int64"}], "rows": [[1], [2]]}`. A list of such tables, one
  table, or an object of `name: rows` is accepted. Rows are lists (by position) or objects (by key). Types are
  inferred unless a column declares one; a declared `int64` or `uint64` accepts exact integers sent as strings, and a
  declared `decimal(p,s)` is held as text (use `CAST(col AS DECIMAL(p,s))` to calculate with it).

`mode` is `tables` (the default without `sql`), `describe`, or `query` (the default with `sql`).

- `tables` lists every table with its columns, types and `row_count`. The row count is exact for Parquet and
  inline tables, and for text and sheets that were read whole while their types were inferred; otherwise it is
  `null`, never a guess. A table that cannot be read is listed with its `error`.
- `describe` gives per column `type`, `non_null`, `nulls`, `distinct`, `min`, `max` and, for numbers, `mean`, over
  the first `sample_rows` rows (default 100000, at most 10000000), plus five sample rows. `sampled` says when the
  table is longer than what was read. `table` picks one table; `columns` picks columns (at most 200 a call).
- `query` runs `sql`, one SELECT (a WITH query, joins, windows, UNION and EXPLAIN of a SELECT are fine). CREATE,
  DROP, INSERT, UPDATE, DELETE, COPY, SET and every other statement, also behind EXPLAIN, are refused with one
  sentence. A query may read no table at all (`SELECT 1 + 1 AS two`), so `path` and `tables` can be left out.

Options: `max_rows` (1 to 100000, default 200) and `max_bytes` (4096 to 3000000, default 1000000) bound one page
of rows; `markdown_rows` (0 to 1000, default 50) bounds the Markdown table (it stops at 256 KiB and says how many rows
it shows); `cursor` continues a result;
`delimiter` (one character or `tab`) and `header` (true or false) override CSV detection.

To save a result as a file, add `output` (see Saving a result).

## SQL

The engine is Apache DataFusion (PostgreSQL-like). Table and column names keep their case: `Sales` and `sales`
are different, and a name with spaces or other characters is written in double quotes (`"Order Date"`). String
literals use single quotes. Dates are `DATE '2024-01-31'`, timestamps `TIMESTAMP '2024-01-31 12:00:00'`.
Useful: `date_trunc`, `extract`, `to_timestamp`, `cast(x AS DECIMAL(18,2))`, `coalesce`, `nullif`, `regexp_like`,
`regexp_replace`, `split_part`, `string_agg`, `approx_distinct`, `median`, `array_agg`, `unnest`; a struct column
is read with `col['field']` and a list with `col[1]`. Integer `+`, `-`, `*` and `SUM` fail with a sentence on
overflow instead of wrapping, in aggregates and in window functions alike; cast to DECIMAL(38,0) first when
sums can pass 64 bits. `repeat`, `lpad`, `rpad`, `range`, `generate_series` and `array_repeat` with a literal size
over 64 MiB of text or 20 million elements are refused (a size that comes from a column is not checked, and a
query that builds more than the tool may use fails with a sentence or is stopped at the memory limit).

CSV types are inferred: integers stay integers (a number with a leading zero, such as a zip code, stays text),
a column becomes float only when a float is in it, `YYYY-MM-DD` is a date, `YYYY-MM-DD[T ]HH:MM[:SS[.f]]` a
timestamp, `true` and `false` booleans. A first row is a header when all its filled cells are text and either no
cell is empty and the names are distinct, or a column below holds numbers, dates or booleans; so `,a,b` over `0,1,2`
(a pandas index column) is a header whose empty cell is named `column_1`, and a table has at most 2000 columns. An empty field is NULL; an empty quoted field is the empty string. Types
come from the first 100000 rows; a later value that disagrees makes the call read the whole table once to settle
the type; when such a table's result has more than one page, the first page already reads it once so that every
page has the same column types. Other date formats are text: use `to_date(col, '%d/%m/%Y')`.

## Output

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

### Row order

A query without ORDER BY still has one repeatable order: a plain scan (filters, projections and LIMIT over one
table) is in the file's row order, and anything else (GROUP BY, JOIN, DISTINCT, window functions, UNION) is
sorted by all its output columns, ascending, NULLs last. `order` says which rule applied: `query`, `file`,
`columns` or `none` (no sortable column, and more than one row). Rows that tie under your own ORDER BY come back in the same order on
every call.

### Reading in pages

A result longer than one page carries `next: {"cursor": "..."}`. Call again with the same fields and
`"cursor": "<that value>"`; the next page starts exactly where this one ended, nothing repeated or skipped,
and you may change `max_rows`. Repeat until `next` is absent. A cursor is refused, with the reason, when the
SQL, the options or the files changed. The query runs again for each page, so a very deep page of a large
sorted result costs a full run: narrow the query instead when you can.

## Saving a result

`output` (a plain file name such as `result.csv`: no folders, not starting with a dot) writes the whole result of
`sql` into the folder `path` instead of returning rows; `path` must be a folder. Only a call with `output` writes,
and the user is asked to allow writing to that folder. `format` is `csv` or `parquet`, taken from the name when it
ends in `.csv` or `.parquet`; an existing file is replaced only with `overwrite: true`. The result is
`{"written": "result.csv", "format": "csv", "rows": 1250, "bytes": 48213, "warnings": []}`, and the file appears
only when it is complete. CSV keeps NULL (an empty field) apart from the empty string (`""`) and floats exact, so
reading it back gives the same rows; Parquet keeps every type. The new file is a table of the folder.

## Limits and errors

The engine's operators (sort, join, grouping) share 1 GiB: a query that needs more fails with a sentence saying
to narrow it with WHERE or LIMIT, select fewer columns, or aggregate first. Reading files is not counted there; it
streams, so a plain scan, filter or aggregate runs in flat memory whatever the file size, and the plugin as a
whole is stopped at its 3 GiB limit. SQL is limited
to 64 KiB and 256 levels of nesting; a row is limited to 16 MiB. A call that fails exits with one sentence.
Files that are not data are skipped and listed in the `warnings` of `tables`; Arrow files are not supported.
A workbook's strings are read only when one of its sheets is named, and a workbook whose strings pass 512 MiB is
refused with a sentence saying to save the sheet as CSV.

## Examples

List what is there: `{"path": "reports"}`

Ask a question: `{"path": "reports", "sql": "SELECT region, sum(units) AS units FROM sales GROUP BY region"}`

Look at a table first: `{"path": "reports/book.xlsx", "mode": "describe"}`

Join two files: `{"path": "reports", "sql": "SELECT r.boss, sum(s.units) AS u FROM sales s JOIN regions r ON s.region = r.region GROUP BY r.boss"}`

Query rows you already have: `{"tables": [{"name": "t", "rows": [{"a": 1}, {"a": 2}]}], "sql": "SELECT sum(a) AS s FROM t"}`
