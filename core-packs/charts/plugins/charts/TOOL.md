# charts

Draws a chart and returns `{"response": {...}, "parts": [...]}`: `response`
holds `chart`, `alt`, `series` (the exact values) and `warnings`, and `parts`
carries the PNG image; `save` writes SVG and PNG files instead. Never invent
numbers: chart only data you were given or read from a file.

Give `chart` and the data: `data` inline (a `data_tables` result as it is, a
list of objects, or CSV text) or `path` (a CSV, TSV, JSON or JSON Lines file,
or its folder with `file`). `x` and `y` name the columns; by default the first
column and every numeric one.

Answer from the picture, `response.alt` and `response.series`. Read `warnings`
before you trust the chart: gaps, cut ranges and reduced series are said there.
A failed call is one sentence saying what to change; fix that, do not retry the
same call.

Help: `{"help": "topics"}` lists the topics (inputs, types, options, formats,
output, saving); `{"help": "<topic>"}` returns one in `response.text`. Check
`types` or `output` for per-chart behavior, limits, and grouping (e.g.
`await tools.charts({help: "types"})`).
