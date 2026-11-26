One JSON object. Name the data in one or both ways:

- `path`: a data file, or a folder of them (a relative path starts at the working folder). Only that file or
  folder is readable for the call. In a folder every readable file, below subfolders too, is a table; hidden
  files are ignored. `files` lists the relative paths inside the folder to use, in that order.
- `tables`: rows sent inline, either a `data_tables` result as it is (`{"response": {"columns": [...], "rows":
  [...]}}`) or `{"name": "t", "columns": [{"name": "a", "type": "int64"}], "rows": [[1], [2]]}`. A list of such
  tables, one table, or an object of `name: rows` is accepted. Rows are lists (by position) or objects (by key). Types are
  inferred unless a column declares one; a declared `int64` or `uint64` accepts exact integers sent as strings, and a
  declared `decimal(p,s)` is held as text (use `CAST(col AS DECIMAL(p,s))` to calculate with it).

`mode` is `tables` (the default without `sql`), `describe`, or `query` (the default with `sql`).

- `tables` lists every table with its columns, types and `row_count`. The row count is exact for Parquet and
  inline tables, and for text and sheets that were read whole while their types were inferred; otherwise it is
  `null`, never a guess. A table that cannot be read is listed with its `error`. `table` picks one table.
- `describe` gives per column `type`, `non_null`, `nulls`, `distinct`, `min`, `max` and, for numbers, `mean`, over
  the first `sample_rows` rows (default 100000, at most 10000000), plus five sample rows. `sampled` says when the
  table is longer than what was read. `table` picks one table; `columns` picks columns (at most 200 a call).
- `query` runs `sql`, one SELECT (a WITH query, joins, windows, UNION and EXPLAIN of a SELECT are fine). CREATE,
  DROP, INSERT, UPDATE, DELETE, COPY, SET and every other statement, also behind EXPLAIN, are refused with one
  sentence. A query may read no table at all (`SELECT 1 + 1 AS two`), so `path` and `tables` can be left out.

Options: `max_rows`, `max_bytes` and `markdown_rows` bound one page and `cursor` continues a result (the paging
topic); `delimiter` and `header` override CSV detection (the csv topic); `output` saves the result as a file (the
saving topic).
