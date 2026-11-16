# charts

Draws a chart from data and returns it as a picture you can look at and a text
description, or saves it as SVG and PNG files. Use it whenever a question is better answered with
a chart than with a table: trends, comparisons, shares, spreads, relations.
Never invent numbers: chart the data you were given or read from a file.

## Input

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
  about its extension still works).

Say which columns to draw:

| Field | Meaning | Default |
| --- | --- | --- |
| `x` | x axis, categories, pie labels | the first column (the row number when that column is also in `y`) |
| `y` | list of value columns, one series each | every numeric column |
| `series` | a column whose values split the rows into series (long data); then `y` is one column | none |
| `size` | bubble size column | none |
| `value` | heatmap cell value column | the first other numeric column |
| `line` | combo charts: columns drawn as lines | none |
| `agg` | how rows with the same x or category combine: `sum`, `mean`, `count`, `min`, `max`, `median` | `sum` (`mean` for heatmaps) |
| `sort` | category order: `none`, `x`, `x_desc`, `value`, `value_desc` | `none` |

### Chart types

| `chart` | Draws | Notes |
| --- | --- | --- |
| `line` | lines through numbers, dates or categories | x may be numbers, ISO dates or text |
| `area` | a filled line from zero | |
| `stacked_area` | areas piled on each other | a series with no value at an x counts as zero there, and says so |
| `bar` | grouped bars; `stack: "stacked"` stacks them, `horizontal: true` lays them down | value labels on up to 30 bars |
| `stacked_bar`, `horizontal_bar` | shorthands for the two options | |
| `scatter` | points; `series` colours groups | each group's correlation is in the result |
| `bubble` | points sized by `size` | bubble area is proportional to the value, from zero; sizes of zero or below are not drawn and are counted |
| `histogram` | counts of one numeric column; `bins` is a number (1 to 200) or `auto` | `series` overlays groups on shared bins |
| `box` | box and whisker per `x` group, or per value column | whiskers reach 1.5 interquartile ranges, outliers are dots |
| `pie`, `donut` | shares of a whole | more than 12 slices: the smallest become "Other" |
| `heatmap` | a colour grid over `x` and `y` categories | sequential colours, or diverging when values cross zero or `center` is set |
| `combo` | bars (`y`) with lines (`line`) on a second axis | `line_axis: "left"` puts the lines on the first axis |

### Options

`stack` (`grouped` or `stacked`) and `horizontal` (`true`) for bar charts;
`line_axis` (`right` or `left`) for combo charts; `title`, `subtitle`; `x_label`, `y_label`, `y2_label` (default: the column
names; an empty string removes one); `x_scale` (`auto`, `linear`, `log`,
`time`, `category`), `y_scale` and `y2_scale` (`linear`, `log`); `x_min`,
`x_max`, `y_min`, `y_max` to fix the visible range (values outside it are cut
off and counted in `warnings`); `legend` (`auto`, `right`,
`bottom`, `top`, `left`, `none`); `theme` (`light`, `dark`); `colors` (hex
list); `width` (200 to 4096) and `height` (150 to 4096), default 800 by 480;
`scale` (PNG pixel density, 0.5 to 4); `output` (`both`, `svg`, `png`).

- A date column (`2024-01-31`, `2024-01-31T10:00:00Z`, with offsets) becomes a
  time axis with calendar ticks. Years like 2019 stay plain numbers, written
  without a separator.
- `format`, `y_format`, `y2_format`, `x_format` take a number format: `,.2f`
  (thousands separators, two decimals), `.1%` (percent), `~s` (1.5k, 2M),
  `d` (whole numbers), `e` (scientific), with optional text around the spec in
  braces: `${,.0f}`, `{.1%}`, `{~s} units`; a symbol directly around the spec
  needs no braces (`$,.0f`, `.1f%`). Exact halves round away from zero (2.5 is
  3, -2.5 is -3), and the minus sign comes before the symbol (`-$1,234`). On a time axis `x_format` is a date
  pattern: `%Y %m %d %H %M %S %b %B`.
- A heatmap's colour bar is on the right whatever `legend` says (`none` removes
  it); an empty cell is a dashed outline with a diagonal, never a colour.
- A title is left out, with a warning, when the picture is too small for it.
- A log axis needs values above zero; a zero or negative value is an error
  that names the first one.
- Pick colours with `colors` only when asked; the default palette stays
  distinguishable for colour-blind readers, in light and dark.

## Output

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

The PNG arrives as an image you can look at, at most 1568 pixels on its long
side. `output: "svg"` returns the SVG text in `response.svg` instead, unless it
is longer than a reply can carry (then use `save`). Answer from what you see
and from `alt` and `series`, which carry the exact values: `alt` says the type,
axes and ranges, and each series' highest and lowest point. Per type, `series` also holds: `drawn`
(marks actually drawn when a series was reduced), box statistics (`q1`,
`median`, `q3`, whiskers, outliers), pie `value` and `share`, histogram
`edges` and `counts`, scatter `correlation`, `x_min` and `x_max`, heatmap
`rows`, `columns` and `empty_cells`.

**Read `warnings` before you trust the chart.** Empty cells and text where a
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

## Saving files

`save` writes the chart into the folder named in `path` (a folder, not a file;
the data is `file` inside it, or inline in `data`). A base name like
`"revenue"` writes `revenue.svg` and `revenue.png` at full size;
`{"svg": "a.svg", "png": "a.png"}` names them (either may be left out); a
subfolder like `"out/revenue"` is created. Names stay inside the folder and an
existing file is replaced. `response.files` lists what was written.

Writing needs the operator's permission for that folder: the first save may
ask, and a refusal is one sentence naming the folder. Tell the user that
sentence; do not retry the same save.

## Behaviour to know

- A single data file or a folder named in `path` is the only thing the call can
  read. A `file` that leaves the folder, is absolute or goes through a
  symbolic link is refused.
- If a detailed picture would not fit the reply, the PNG is drawn at a
  smaller scale, with a warning; saved files stay complete.
- One image is at most 16 million pixels (`width` x `height` x `scale`
  squared); a larger request is refused with that number.
