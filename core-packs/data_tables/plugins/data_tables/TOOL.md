# data_tables

Runs SQL over CSV, TSV, JSON, NDJSON, Parquet, XLSX and ODS files of any size, a folder of them, or inline
rows, and returns `{"response": {...}}`: `response` holds `columns`, `rows`, `markdown` and `warnings`
(or `tables` / `written`). Let SQL count, filter, group and join instead of reading the raw file.

- `{"path": "reports"}` lists tables and columns.
- `{"path": "reports/book.xlsx", "mode": "describe"}` gives column statistics and sample rows.
- `{"path": "reports", "sql": "SELECT region, sum(units) AS units FROM sales GROUP BY region"}` runs one
  SELECT (Apache DataFusion; names keep their case, quote `"Order Date"`).
- `tables` sends inline rows (or a `data_tables` result as it is) instead of `path`.

Read `warnings` (`response.warnings`): cut values, skipped files and short pages are said there. A result
with `next` continues with its `cursor` and every other field unchanged. `output` saves to the folder
`path`. A failure is one sentence; fix what it names, never repeat the call unchanged.

Help: `{"help": "topics"}` lists the topics (inputs, sql, csv, output, paging, saving, limits);
`{"help": "<topic>"}` returns one in `response.text`.
