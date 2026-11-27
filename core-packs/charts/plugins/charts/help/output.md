```json
{"response": {
   "chart": "bar", "width": 800, "height": 480,
   "alt": "Bar chart titled ... 4 categories ... highest 18 at West ...",
   "series": [{"name": "q1", "mark": "bar", "color": "#0072b2", "axis": "left",
               "points": 4, "gaps": 0, "min": 8, "max": 18, "sum": 52}],
   "warnings": [],
   "png": {"width": 800, "height": 480, "bytes": 21345}},
 "parts": [{"type": "image", "data": "<base64>", "mimeType": "image/png"}]}
```

Every tool call returns its fields inside `response` (the same `{"response": {...}}` envelope `data_tables`
uses), with the PNG in `parts`. The PNG arrives as an image you can look at, at most 1568 pixels on its long
side; if a detailed picture would not fit the reply, it is drawn at a smaller
scale, with a warning. `output: "svg"` returns the SVG text in `response.svg`
instead, unless it is longer than a reply can carry (then use `save`). Answer
from what you see and from `response.alt` and `response.series`, which carry the exact values:
`alt` says the type, axes and ranges, and each series' highest and lowest
point. Per type, `series` also holds: `drawn` (marks actually drawn when a
series was reduced), box statistics (`q1`, `median`, `q3`, whiskers,
outliers), pie `value` and `share`, histogram `edges` and `counts`, scatter
`correlation`, `x_min` and `x_max`, heatmap `rows`, `columns` and
`empty_cells`.

Read `warnings` before you trust the chart. Empty cells and text where a
number should be are drawn as gaps, never as zero, and counted. Charts reduce
what they draw and say so: lines longer than 1500 points keep their first,
last, lowest and highest points; scatter clouds over the cap are grouped into
marks (darker means more points) with the extreme points exact; more than 24
series, 100 categories, 12 pie slices, 40 boxes or 80 heatmap rows or columns
are cut at that number. Characters the built-in font lacks (many CJK and emoji
characters) show as boxes and are listed.

A call that cannot draw fails with one sentence that says what to change, such
as `column "revnue" is not in the data; the columns are month, revenue`.
Fix that and call again; do not retry the same call.
