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

Examples:

- List what is there: `{"path": "reports"}`
- Ask a question: `{"path": "reports", "sql": "SELECT region, sum(units) AS units FROM sales GROUP BY region"}`
- Look at a table first: `{"path": "reports/book.xlsx", "mode": "describe"}`
- Join two files: `{"path": "reports", "sql": "SELECT r.boss, sum(s.units) AS u FROM sales s JOIN regions r ON s.region = r.region GROUP BY r.boss"}`
- Query rows you already have: `{"tables": [{"name": "t", "rows": [{"a": 1}, {"a": 2}]}], "sql": "SELECT sum(a) AS s FROM t"}`
