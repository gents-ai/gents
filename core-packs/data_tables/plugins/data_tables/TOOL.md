# data_tables

Runs SQL over CSV, TSV, JSON, NDJSON, Parquet, XLSX and ODS files of any size, a folder of them, or inline
rows, and returns exact results. Let SQL count, filter, group and join instead of reading the raw file.

- `{"path": "reports"}` lists the tables and their columns.
- `{"path": "reports/book.xlsx", "mode": "describe"}` gives column statistics and sample rows.
- `{"path": "reports", "sql": "SELECT region, sum(units) AS units FROM sales GROUP BY region"}` runs one
  SELECT (Apache DataFusion; names keep their case, quote `"Order Date"`).
- `tables` sends rows inline instead of `path`.

Read `warnings`: cut values, skipped files and short pages are said there. A result with `next` continues
with its `cursor` and every other field unchanged. `output` saves the result as a file in the folder `path`.
A failure is one sentence; fix what it names, never repeat the call unchanged.

Help: `{"help": "topics"}` lists the topics (inputs, sql, csv, output, paging, saving, limits);
`{"help": "<topic>"}` returns one.
