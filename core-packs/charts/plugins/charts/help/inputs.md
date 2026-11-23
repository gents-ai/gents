One JSON object. `chart` is required; everything else has a default.

Give the data in one of two ways:

- `data`: inline, up to a few thousand rows. Three shapes work: the result of
  the `data_tables` tool as it is, `{"columns": ["month", "sales"], "rows":
  [["Jan", 10], ["Feb", 12]]}`; a list of objects, `[{"month": "Jan", "sales":
  10}]`; or CSV text with a header line.
- `path`: a CSV, TSV, JSON or JSON Lines file, or the folder holding it, with
  `file` naming the file inside the folder. A relative path starts at the
  working folder. Files of any size are read in one streaming pass; only the
  columns the chart uses are kept, and the result says so when it stops at the
  2 000 000 row limit. The format is decided by the content (a name that lies
  about its extension still works). A `file` that leaves the folder, is
  absolute or goes through a symbolic link is refused.

Say which columns to draw:

| Field | Meaning | Default |
| --- | --- | --- |
| `x` | x axis, categories, pie labels | the first column (the row number when that column is also in `y`) |
| `y` | list of value columns, one series each; for a heatmap, the column on the other axis | every numeric column |
| `series` | a column whose values split the rows into series (long data); then `y` is one column | none |
| `size` | bubble size column | none |
| `value` | heatmap cell value column | the first other numeric column |
| `line` | combo charts: columns drawn as lines | none |
| `agg` | how rows with the same x or category combine: `sum`, `mean`, `count`, `min`, `max`, `median` | `sum` (`mean` for heatmaps) |
| `sort` | category order: `none`, `x`, `x_desc`, `value`, `value_desc` | `none` |
