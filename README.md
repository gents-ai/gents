# Charts pack

Turns data into charts a model and a person can both read: line, area,
stacked area, bar (grouped, stacked, horizontal), scatter, bubble, histogram,
box plot, pie, donut, heatmap and bar-plus-line combinations with a second
axis. A chart comes back as a PNG image the model (and the desktop) sees, the
SVG it was drawn from, and a text description with the numbers that matter, so
a model without vision can still say what the chart shows. The pack ships two
plugins, `charts` (draws and returns) and `charts_save` (also writes the files
into a folder), both WebAssembly modules that draw byte-identical pictures on
every operating system gents runs on, and a ready-made **Chart maker** agent
that uses them.

Data comes inline (the `{"columns": [...], "rows": [[...]]}` shape the
data_tables pack returns, a list of objects, or CSV text) or from a CSV, TSV,
JSON or JSON Lines file of any size in the working folder. Empty and
non-numeric values are drawn as gaps, never as zero, and counted in
`warnings`.

## Use it from the desktop

No configuration.

1. Install `gents/charts` from the Packs panel.
2. Pick **Chart maker** as the agent for a chat.
3. Ask: "Chart revenue by month from `sales.csv` as a line per region", or
   paste a few rows and ask for a pie.

Files in the working folder of the chat are readable at once (the folder you
gave the agent as its tool root; never `/` or your home folder, which a
launcher may use as a current directory). A file elsewhere raises "Allow
charts to read `<path>`?". Writing files is a separate tool, `charts_save`, and
asks for write access to the folder the first time (see Authority). The same
agent runs from a terminal:

```sh
gents pack install gents/charts --inference-slot chart_maker=<profile>
gents chat --behavior-id chart-maker "Chart sales.csv: revenue by month, one line per region"
```

## Use it as a model tool

The `chart-maker` behavior's Tools document grants both plugins
(`integrations.plugins: [{"plugin": "gents/charts"}, {"plugin":
"gents/charts_save"}]`). To give the tool to another behavior, add the entry to
that behavior's Tools:

```json
{"tools_id": "my-tools", "integrations": {"plugins": [{"plugin": "gents/charts"}]}}
```

The model calls `charts`:

```json
{"chart": "line", "path": "sales.csv", "x": "month", "y": ["revenue"],
 "series": "region", "title": "Revenue by region", "format": ",.0f"}
```

and gets back the picture as an image part, the SVG, `alt` (a description with
each series' highest and lowest point), `series` (the numbers behind the
marks) and `warnings`. `plugins/charts/TOOL.md` is what the model reads.

## Use it as a graph node

Installing the pack also installs two plugin nodes as plain callbacks, so no
model and no graph pack is needed: create a `ChartRequest` document and a
`ChartResult` appears.

```sh
gents server --home <home> --http-port 8080 &   # started in the folder holding your files
gql() { curl -fsS http://127.0.0.1:8080/api/v0/graphql -H 'content-type: application/json' -d "$(jq -cn --arg q "$1" '{query: $q}')"; }
gql 'mutation { create_ChartRequest(input: {run_id: "c1", chart: "bar", title: "Revenue", x: "month", y: ["revenue"], path: "reports", file: "sales.csv"}) { _docID } }'
gql '{ ChartResult(filter: {run_id: {_eq: "c1"}}) { chart width height alt png_base64 warnings error } }'
```

A request is recognized as a node's by the `run_id` every such document
carries. `data` may hold rows as JSON text or CSV text, so the output of a
data_tables node can be passed straight in.

| Node | Reads | Writes |
| --- | --- | --- |
| `chart-render` (plugin `charts`) | a `ChartRequest` with no `save` | one `ChartResult` |
| `chart-save` (plugin `charts_save`) | a `ChartRequest` with `save` set | one `ChartResult` whose `svg_file` and `png_file` name the files written |

`ChartRequest` has the fields of the tool (`chart`, `data`, `path`, `file`,
`x`, `y`, `line`, `series`, `size`, `value`, `agg`, `sort`, `stack`,
`horizontal`, `bins`, `title`, `subtitle`, `x_label`, `y_label`, `y2_label`,
`x_scale`, `y_scale`, `y2_scale`, `format`, `x_format`, `y_format`,
`y2_format`, `legend`, `theme`, `colors`, `line_axis`, `width`, `height`,
`scale`, `x_min`, `x_max`, `y_min`, `y_max`, `center`, `output`, `save`).
`ChartResult` carries:

| Field | Meaning |
| --- | --- |
| `chart`, `width`, `height` | what was drawn |
| `alt` | the text description |
| `svg` | the SVG document (left out, with a warning, when the record would pass 2 MB; a `png`-only request has none) |
| `png_base64`, `png_width`, `png_height` | the PNG |
| `svg_file`, `png_file` | files written by `chart-save` |
| `series_json` | the `series` list as JSON text |
| `warnings` | what was reduced, left out or drawn as gaps |
| `error` | the one-sentence reason when nothing could be drawn; the record still exists, so a run never stalls on a bad chart |

A path in a graph request must be inside the server's working folder or an
allowed folder (`gents plugin dirs add <folder>`; `--access read_write` for
`chart-save`): a graph node asks nobody and is refused with that command
otherwise.

## Use it from the CLI

```sh
gents pack install gents/charts --home <home> --inference-slot chart_maker=<profile>
gents plugin run charts --home <home> --bind-dir ./reports \
  --input '{"chart": "bar", "file": "sales.csv", "x": "month", "y": ["revenue"], "title": "Revenue"}' \
  > chart.json
jq -r '.response.svg' chart.json > chart.svg
jq -r '.parts[0].data' chart.json | base64 -d > chart.png
jq -r '.response.alt' chart.json
```

`--bind-dir` names the folder (or one file) the call may use. With `charts` it
is read-only. To get the files written, run `charts_save` the same way, which
gets the folder read and write:

```sh
gents plugin run charts_save --home <home> --bind-dir ./reports \
  --input '{"chart": "bar", "file": "sales.csv", "x": "month", "y": ["revenue"], "save": "revenue"}'
ls reports   # revenue.svg  revenue.png  sales.csv
```

## Installation

`gents pack install gents/charts` installs the Chart maker behavior, its Tools
document and the two plugins, and binds the `chart_maker` inference slot to a
profile (any capable one; a profile that accepts images also checks the
picture). Nothing else needs configuring: there is no network, key or model
download.

## Authority

A call reads only what it names. `charts` declares `bind_dir` (input field
`path`, access `read`): a file it names is the only file the call can see (a
private folder holding one hard link, nothing copied), and a folder it names is
that folder. Inline data needs no path at all and runs fully sealed. Allowed
without asking: the session's working folder (read-only), but only a specific
folder, never `/`, your home folder or a folder holding either. Anything else
asks first in a chat or is refused with the command that allows it in a
headless run (see the OCR pack's Authority section for the full model).

`charts_save` declares `access: read_write` and needs a folder (not a file)
the operator allowed to be written: the working folder is read-only by
default, so the first save asks, and a headless run is refused with
`gents plugin dirs add <folder> --access read_write`. Files are named inside
that folder only: `..`, absolute paths, backslashes and symbolic links are
refused before anything is written, and each file is written whole under a
temporary name and renamed. A call of the read-only `charts` plugin that asks
to save fails with a sentence saying the folder is read-only; the sandbox, not
the plugin, enforces that.

Both plugins have no network, environment or host-tool access. Their `limits`
are 1024 MiB of memory, a 300 s wall clock and 4 MiB of output (the host's
ceiling).

## Inputs and outputs

See `plugins/charts/TOOL.md` for every field, the number-format language and
the chart types. In short:

- One JSON object in; `chart` is required. Data is `data` (inline) or `path`
  (file or folder) with `file`.
- A tool call returns `{"response": {...}, "parts": [{"type": "image", ...}]}`:
  gents turns `parts` into an image the model and the desktop show.
  `response` has `chart`, `width`, `height`, `alt`, `series`, `warnings`, `svg`,
  `png` (size) and, for `charts_save`, `files`.
- A graph node returns one flat `ChartResult` (see above).
- `output` picks `both`, `svg` or `png`; `scale` sets the PNG density; the
  picture is at most 16 million pixels.

### Chart types and what they check

| Chart | Notes |
| --- | --- |
| `line`, `area`, `stacked_area` | numeric, date (calendar ticks) or category x; gaps break the line; lines over 1500 points are reduced with largest-triangle-three-buckets, which keeps the first, last, lowest and highest points |
| `bar`, `stacked_bar`, `horizontal_bar` | zero baseline, negatives hang below it, stacks add positives and negatives separately, value labels on up to 30 bars |
| `scatter`, `bubble` | log axes, series by column, correlation per series; clouds over the cap are grouped into marks with the extreme points kept exact; bubble area is proportional to the value from zero (sizes of zero or below are not drawn and are counted) |
| `histogram` | nice bin edges or an exact bin count, overlays by series |
| `box` | quartiles by linear interpolation, Tukey whiskers, outliers listed |
| `pie`, `donut` | slice angles sum to 360 degrees; small slices grouped as "Other" past 12 |
| `heatmap` | sequential or diverging colours, value labels in small grids, colour bar |
| `combo` | bars on the left axis, lines on the right (or left) |

### Bounds

Every cap is stated in `warnings` when it applies: 2 000 000 rows or 192 MiB
per table, 512 columns, a cell of at most 65 536 bytes (a larger one means the
file is not a table), 24 series, 100 categories per axis, 80 heatmap rows or
columns, 12 pie slices, 40 boxes, 1500 drawn points per line, 10 000 drawn
scatter marks, 2000 bubbles, 200 histogram bins, 16 million pixels, and a
reply under 4 MiB, or a graph record under 2 MB (the SVG text is left out first, then the PNG is drawn
smaller). A table is read in one streaming pass and only the columns the chart
names are kept, so memory follows the chart, not the file.

## Completion and failure

A call either draws or fails with one plain sentence that says what to change:
`column "revnue" is not in the data; the columns are month, revenue`, `the y
axis is logarithmic but column "v" has 1 values at or below zero (first is 0 in
row 2); remove them or use a linear axis`, `the file looks binary, not CSV or
JSON data`, `the chart is too small for its labels and legend; make it larger,
shorten the labels or move the legend`. As a tool or CLI call the plugin exits
non-zero with that sentence; as a graph node the sentence is the `error` of a
`ChartResult`. Partial results are never silent: what a chart left out, reduced
or could not draw is in `warnings`.

## Validation

```sh
gents pack check ./packs/gents/charts
gents pack test ./packs/gents/charts
GENTS=gents scripts/test-pack.sh packs/gents/charts
cargo test --manifest-path packs/gents/charts/plugins/charts/Cargo.toml
```

## Operational history

1.0.0: first release.

## Formats

| Input | Notes |
| --- | --- |
| CSV | RFC 4180 quoting, CRLF or LF, a UTF-8 byte order mark, the delimiter (`,` `;` tab `\|`) sniffed from the header; blank lines skipped; rows with a different number of fields counted in `warnings` |
| TSV | the same reader |
| JSON | an array of objects; `{"columns": [...], "rows": [[...]]}` with list or object rows (columns may be names or `{"name": ...}`); either wrapped in `response`, `result`, `table`, `data` or `output` |
| JSON Lines | one object per line |
| inline text | JSON, or CSV when it does not start with `[` or `{` |

The content decides, not the name: a `.png` that holds CSV text is read as CSV,
and a binary file is refused. Numbers are plain decimal text (`12`, `-3.5`,
`1e3`); text with a leading zero (`007`) stays an identifier. Empty cells are
empty, and so are `null`, `NA`, `N/A`, `NaN` and `None` next to numbers or dates;
in a text column they are ordinary labels (the country code NA stays a
category). In a semicolon-delimited file `1,5` and `1.234,5` are read as
numbers with a decimal comma, and the count is in `warnings`. Dates are `YYYY-MM-DD`,
`YYYY-MM` or a timestamp with `T` or a space and an optional `Z` or offset.

| Output | Notes |
| --- | --- |
| SVG | `<title>` and `<desc>` carry the chart name and description; tooltips (`<title>` children) carry values and the full text of shortened labels; text is real text in Liberation Sans, so a viewer without it falls back to the metric-compatible Arial or Helvetica |
| PNG | opaque RGB, drawn by resvg from the same SVG, so the two always agree |

## Determinism

Every coordinate is formatted with fixed decimals; the logarithm, sine and
cosine behind log axes and pie slices are computed with plain arithmetic, so
they do not depend on the platform's math library; the font is embedded; no
clock, randomness or hash-map order reaches the output. The plugin runs as
WebAssembly under the host, so Linux x86_64, Linux aarch64 and macOS give the
same bytes (the pack's CI runs the cases on all three).

## Performance

Not benchmarked. One sanity check was run once through the host on Linux
x86_64: a 29 MB CSV of 2 000 000 rows drawn as a line chart (`gents plugin run
charts --bind-dir ...`) finished in 3.2 s wall clock with a 298 MiB peak
resident set for the whole gents process, inside the plugin's declared limits
(1024 MiB, 300 s). The line was drawn from 1501 of those points. Everything
else (other chart types, other sizes, other machines) is not measured.

## Models and licences

No model. The plugins embed Liberation Sans (Regular and Bold, 2.1.5), licensed
under the SIL Open Font License 1.1, unchanged and committed in
`plugins/charts/fonts/` with its licence text. The full notice is in
[notice.md](notice.md).

| Font | Source | SHA-256 |
| --- | --- | --- |
| `LiberationSans-Regular.ttf` | https://github.com/liberationfonts/liberation-fonts/files/7261482/liberation-fonts-ttf-2.1.5.tar.gz | `76d04c18ea243f426b7de1f3ad208e927008f961dc5945e5aad352d0dfde8ee8` |
| `LiberationSans-Bold.ttf` | the same archive | `788abee4c806d660e8aee46689dd8540cd4bb98da03dcc9d171ce3efd99a9173` |

To fetch them again and check them:

```sh
curl -fL -o lib.tgz https://github.com/liberationfonts/liberation-fonts/files/7261482/liberation-fonts-ttf-2.1.5.tar.gz
tar xzf lib.tgz && shasum -a 256 liberation-fonts-ttf-2.1.5/LiberationSans-{Regular,Bold}.ttf
```

## Tests and tooling

- Native unit tests sit beside every module in `plugins/charts/source/`:
  geometry read back from the SVG for every chart type (bar heights against
  the axis labels, stacked tops against column sums, pie angles against
  shares, box quartiles against known values, log axes, time ticks), layout
  (no overlapping labels, legends, shortened labels with their full text),
  text (markup injection, control characters, CJK, emoji and right-to-left
  text, huge strings), data sources (every format, wrong extensions, `..`,
  absolute paths and symbolic links), the PNG (size, background, colours at
  known coordinates, a hash of the decoded pixels for one chart per type),
  property tests with a fixed seed (ticks are monotonic, nice and cover the
  data; every point maps inside the plot; stacked tops equal column sums;
  reduction keeps the first, last, lowest and highest points; rendering is
  deterministic; CSV and JSON round trips), and a mutation fuzzer that corrupts
  every fixture and every request with a fixed seed.
- `plugins/charts/tests/*.json` and `plugins/charts_save/tests/*.json` are
  plugin cases with exact expected output (every chart type, every input
  format, inline and bound modes, graph records, and the failures that come
  back as records), run through the real WebAssembly host by `gents pack test`
  and natively by `cargo test`. The case format cannot expect a failed call, so
  failures of the tool itself are pinned by `cargo test`. `host-only-*` cases
  depend on the sandbox and run only in the host.
- `tests/install.json` (install, reinstall, remove), `tests/content.json`
  (what the manifest and configuration promise) and `tests/graph_render.json`
  (a `ChartRequest` created in a served pack becomes a `ChartResult`).
- Fixtures are committed and reproducible: `cargo run --example gen_fixtures`
  (from `plugins/charts`) rewrites them byte for byte, which a test checks.
  `cargo run --release --example bless_cases -- tests ../charts_save/tests`
  records the expected output of new cases; review the pictures before
  committing, then let the host reproduce them.

## What this pack does not do

- Text is laid out left to right with simple advances and no kerning: scripts
  that need shaping (Arabic joining, Indic conjuncts) are drawn without it, and
  characters the font lacks (CJK, emoji) are drawn as empty boxes and listed in
  `warnings`.
- A chart is one bounded image: there is no cursor. Data over the row limit is
  cut at the limit and the result says so; draw a filtered or aggregated
  extract to see the rest.
- No interactive charts, no animation, no dual time zones: times are UTC.
- A chart with more than 24 series, 100 categories or 12 pie slices is reduced
  to that, not paged.
