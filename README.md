# data_tables pack

SQL over the data files people actually have: CSV, TSV, JSON (an array of records, or one record per line),
Parquet, and XLSX and ODS spreadsheets, a folder of them, or rows another tool produced. Results keep exact
types, page deterministically and come with a Markdown table the model can read. The pack ships the
`data_tables` plugin (a WebAssembly module running Apache DataFusion, so it behaves the same on every
operating system gents runs on), a second plugin `data_tables_export` that writes a result to a file, and a
ready-made **Data analyst** agent that uses them.

| Input | Notes |
| --- | --- |
| CSV, TSV, delimited text | delimiter and header detected (or set), quoted fields and newlines, UTF-8 BOM, UTF-16 with a byte order mark; an empty field is NULL, an empty quoted field is the empty string |
| JSON, NDJSON | an array of records or any number of records one after another; nested objects are structs, arrays are lists |
| Parquet | uncompressed, snappy, gzip, brotli, lz4 and zstd; only the columns a query names are read |
| XLSX, ODS | every sheet is a table; number, date, boolean and text cells keep their type; a header row with an empty cell (a pandas index column) is a header |
| inline `tables` | rows as JSON in the shape this plugin returns, so one call's result is the next call's input |

| Mode | What it does |
| --- | --- |
| `tables` | lists tables with columns, types and the row count when it is known without a scan |
| `describe` | per column type, non-null, distinct, min, max, mean over a sample, and sample rows |
| `query` | runs one SELECT; a page of rows with exact types, `markdown`, `next.cursor` and `warnings` |
| `export` | `data_tables_export` only: writes a result as CSV or Parquet into the bound folder |

## Use it from the desktop

No configuration.

1. Install `gents/data_tables` from the Packs panel.
2. Pick **Data analyst** as the agent for a chat.
3. Ask about a file: "Which region sold the most in `reports/sales.csv`?" or "Compare the two sheets of
   `budget.xlsx`".

Files in the working folder of the chat are readable at once; a file elsewhere raises "Allow data_tables to
read `<path>`?". The tool sees only the file or folder the question names. Saving a result with
`data_tables_export` asks to allow writing to that folder, because that is the only tool that writes.

From a terminal:

```sh
gents pack install gents/data_tables --inference-slot data_analyst=<profile>
gents chat --behavior-id data-analyst "Which region sold the most in reports/sales.csv?"
```

## Use it as a model tool

The `data-analyst` behavior's Tools document grants both plugins as model tools
(`integrations.plugins: [{"plugin": "gents/data_tables"}, {"plugin": "gents/data_tables_export"}]`). To give
them to another behavior, add the same entries to that behavior's Tools. The model calls

```json
{"path": "reports", "sql": "SELECT region, sum(units) AS units FROM sales GROUP BY region"}
```

and reads `rows`, `markdown` and `warnings`; a result with `next.cursor` is continued by calling again with that
`cursor` and every other field unchanged. `plugins/data_tables/TOOL.md` is what the model reads.

## Use it as a graph node

Installing the pack also installs one plugin node, `data-run`, wired as a plain callback, so no model and no
graph pack is needed: create a `DataJob` document and the records below appear.

```sh
gents server --home <home> --http-port 8080 &   # started in the folder holding your files
gql() { curl -fsS http://127.0.0.1:8080/api/v0/graphql -H 'content-type: application/json' -d "$(jq -cn --arg q "$1" '{query: $q}')"; }
gql 'mutation { create_DataJob(input: {run_id: "r1", path: "reports", sql: "SELECT region, sum(units) AS units FROM sales GROUP BY region"}) { _docID } }'
gql '{ DataRow(filter: {run_id: {_eq: "r1"}}) { row values } }'
```

`path` names a folder or one file; a relative path starts at the server's working folder, which is readable
without asking, and a path elsewhere must be in the allowed folders (`gents plugin dirs add <folder>`). A request
is recognized as a node's by the `run_id` every such document carries. The node never writes (it binds read-only).

| `DataJob` field | Meaning |
| --- | --- |
| `run_id` | the run; every record below carries it |
| `mode` | `tables`, `describe` or `query` (default `query` with `sql`, else `tables`) |
| `path`, `files`, `table`, `columns` | the data and what to look at, as for the tool |
| `tables` | inline tables as JSON text, so a previous node's `columns` and rows can be queried |
| `sql`, `max_rows`, `cursor` | the query, the page size (at most 5000 rows) and the cursor of the previous page |
| `delimiter`, `header`, `sample_rows`, `max_bytes`, `markdown_rows` | options as for the tool |

| Record | Fields (every record also has `run_id`) |
| --- | --- |
| `DataResult` (one) | `mode`, `complete`, `cursor` (when not complete), `columns` (JSON text of `[{name, type}]`), `row_count`, `offset`, `order`, `markdown`, `warnings`, `error` (the sentence a failed job ends with; the run goes on) |
| `DataRow` (query) | `row` (0-based over the whole result), `values` (JSON text of the row's cell list) |
| `DataTable` (tables, describe) | `table`, `source`, `format`, `sheet`, `row_count`, `columns` (JSON text), `profile` (describe: JSON text of the statistics and sample), `error` |

`DataResult.columns` and the rows' `values` are exactly the `columns` and `rows` the plugin returns, so a node
that wants a chart passes them on unchanged (see "Combine it" below). The collections' schemas are in `schemas/`.

## Use it from the CLI

`gents plugin run` binds the folder you name for one call:

```sh
gents plugin run gents/data_tables --bind-dir ./reports \
  --input '{"sql": "SELECT region, sum(units) AS units FROM sales GROUP BY region"}' | jq -r .markdown
```

A long result is read to the end with the cursor:

```sh
dir=./reports sql='SELECT * FROM sales' cursor=""
while :; do
  input="$(jq -nc --arg s "$sql" --arg c "$cursor" '{sql: $s} + (if $c == "" then {} else {cursor: $c} end)')"
  out="$(gents plugin run gents/data_tables --bind-dir "$dir" --input "$input")"
  jq -c '.rows[]' <<<"$out"
  cursor="$(jq -r '.next.cursor // empty' <<<"$out")"
  [ -n "$cursor" ] || break
done
```

To write a result, run `gents plugin run gents/data_tables_export --bind-dir ./reports --input
'{"sql": "SELECT * FROM sales", "output": "copy.parquet"}'`.

## Combine it

- **ocr**: the ocr pack reads CSV and XLSX into Markdown for a model; this pack answers questions over the same
  files exactly. Use ocr to read a scanned or office document, then query the tables it exposes (export them to
  CSV, or pass the rows inline).
- **charts**: the `columns` and `rows` of a result are the input shape of the charts pack, unchanged; feed them
  on directly, or through `DataResult.columns` and `DataRow.values` in a graph.
- **itself**: `tables` accepts the `columns` and `rows` of an earlier result, so a query can read a previous
  query's output without a file.

## Exact results

Integers are JSON numbers, except an `int64` or `uint64` beyond +-2^53, which is a string so a JavaScript reader
cannot round it, and every decimal is a string; the column `type` says which. Floats print in their shortest form
that reads back identically; NaN, Infinity and -Infinity are those strings, and `warnings` says when a page has
any. NULL is `null` and the empty string is `""`. Dates and timestamps are ISO-8601. Integer `+`, `-`, `*` and
`SUM` fail on overflow instead of wrapping (division by zero and out-of-range casts fail too); `AVG`, standard
deviation and other statistics use floating point as SQL does.

## Row order and paging

A query with its own ORDER BY keeps it. Without one, a plain scan keeps the file's row order, and every other
shape (aggregates, joins, DISTINCT, windows, UNION) is sorted by all its output columns, ascending with NULLs
last, so the same query always returns the same rows in the same order; `order` in the result says which rule
applied. A page is rows `offset` to `offset + n` of that order. The cursor holds a fingerprint of the SQL, the
options, and every table the query opened (its name, size, its first and last 64 KiB and 64 evenly spaced 4 KiB
blocks between them, so a file of up to 384 KiB is covered whole; an edit that keeps the size of a larger file and
misses every sampled block is not seen, and the clock is left out so the same file always gives the same cursor),
and is refused with the reason when any changed. A table whose types came from a sample is read once in full when
its result has a next page, so every page has the same column types. The query is run again
for each page and the rows before the page are dropped as they stream, so memory stays flat and the cost of a page
grows with its offset; narrow the query, or export the result, for a very long one.

## Bounded memory, any size

Scans stream: a multi-GiB CSV, JSON or Parquet file is read with flat memory for a scan, a filter, an aggregate or
a LIMIT. Parquet reads only the columns a query names, one row group at a time (a row group over 256 MiB in the
columns read is refused with a sentence). The engine's operators (sort, join, hash aggregate) share a 1 GiB pool
and never spill to disk, so a query that cannot fit fails with a sentence naming what to narrow instead of being
killed. CSV and JSON types come from the first 100000 rows; a later value that disagrees makes the call read the
table once in full to settle the type, and it still returns the right answer. SQL is limited to 64 KiB and 256
levels of nesting, a row to 16 MiB, a text value in a result to 64 KiB (said in `warnings`), and a result page to
3 MB of rows. The whole result (rows, Markdown, columns and warnings) is held under 3.5 MB: a page that would
pass it holds fewer rows and says so, a value over 64 KiB is cut at that size (a list or record as the first 64 KiB
of its JSON text, never built whole), and a row over 1 MiB has its long texts shortened. A table has at most 2000
columns: the engine plans a wider one in time that grows faster than its width, so it is refused with a sentence.
A workbook is opened only when a query names one of its sheets, and its strings are read as a stream and capped at
512 MiB. `repeat`, `lpad`, `rpad`, `range`, `generate_series` and `array_repeat` with a literal size over 64 MiB of
text or 20 million elements are refused before they run. The plugin's declared limits are 3072 MiB of memory, a
900 s wall clock and 4 MiB of output.

## Authority

A call reads only what it names. The `data_tables` plugin declares `bind_dir` (input field `path`, access `read`):
the file it names is the only file the call can see (a private folder holding one hard link, nothing copied), and
a folder it names is that folder. The plugin has no network, environment or write access. `data_tables_export`
declares the same binding with access `read_write` and is the only tool that can write: it writes one named file
in the folder, under a hidden temporary name that replaces the target only when the whole result was written, and
never replaces an existing file unless `overwrite` is true. SQL cannot reach other files: only `SELECT` is
accepted (CREATE EXTERNAL TABLE, COPY, DDL and DML are refused), and the sandbox shows the plugin nothing else. A
symbolic link that leads outside the folder is not followed, and `files` entries with `..` or a leading `/` are
refused.

## Installation

```sh
gents pack install ./packs/gents/data_tables --home <home> --inference-slot data_analyst=<profile>
gents pack install gents/data_tables --home <home> --inference-slot data_analyst=<profile>   # once published
```

Building from a source checkout compiles the plugin to `wasm32-wasip1` with the compiler `rust-toolchain.toml`
pins, so it needs a Rust toolchain and `rustup target add wasm32-wasip1`; a pack fetched pre-built skips the
build.

## Not done

- Arrow IPC files are not read: the Arrow reader panics on damaged input and a plugin cannot recover from a
  panic, so the pack refuses them with a sentence instead of risking a crash. Save such data as Parquet.
- Parquet row-group statistics and page indexes are not used to skip data; every row group of the columns read is
  scanned (the memory stays flat, the time does not shrink).
- A page costs a run of the query up to its offset (see above); there is no server-side cursor state.
- Other date formats than ISO-8601 stay text in CSV; convert with `to_date`.
- Excel formulas read as the value Excel stored; there is no recalculation. XLS (the pre-2007 format) is not read.
- A size that comes from a column (`repeat('a', col)`) is not checked before the function runs; the engine's pool
  does not see what one function builds, so such a query is stopped by the plugin's memory limit.
- ODS files are listed with one streamed pass over each file's content (flat memory, time that grows with the
  file), because the sheet names live there; XLSX files are listed from their small workbook part only.
- A cursor does not carry file modification times (they would make the same file give different cursors on
  different machines), so a same-size edit that misses every sampled block is not seen.
- Nothing here was benchmarked for this pack; the only timing check is that realistic inputs run inside the
  declared limits.

## Tests

`scripts/test-pack.sh packs/gents/data_tables` builds the pack, runs `gents pack test` (every case in
`plugins/data_tables/tests/`, byte-exact, through the real WebAssembly host), the plugin's own `cargo test`, the
install case and the graph cases. The fixtures under `plugins/data_tables/tests/fixtures` are written by
`cargo run --example gen_fixtures` and are identical when it runs again.
