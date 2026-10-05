# charts_save

The `charts` tool with the right to write: it draws the same charts from the
same fields (`chart`, `data` or `path` and `file`, `x`, `y`, `series`, `title`,
`format`, `theme`, `width`, `height` and the rest of the `charts` fields) and
also saves the picture as files in a folder. Use it only when the user wants
files; for looking at a chart, use `charts`.

## Input

All the `charts` fields, and:

- `path`: the folder to write into. It must be a folder, not a file; the data
  file, if there is one, is `file` inside it. Data may also be inline in `data`.
- `save`: what to write, inside that folder. A base name like `"revenue"`
  writes `revenue.svg` and `revenue.png` (a base name that already ends in
  `.svg` or `.png` is used without it); `{"svg": "a.svg", "png": "a.png"}`
  names them (either may be left out); a subfolder like `"out/revenue"` is
  created when missing. Names are relative, end in `.svg` or `.png` and stay
  inside the folder: `..`, absolute paths, backslashes and symbolic links are
  refused. An existing file of the same name is replaced.

```json
{"chart": "line", "path": "reports", "file": "sales.csv", "x": "month",
 "y": ["revenue"], "title": "Revenue", "save": "revenue"}
```

## Output

The `charts` result, plus `response.files`: `[{"path": "revenue.svg",
"bytes": 4812}, {"path": "revenue.png", "bytes": 21345}]`. The files hold
exactly the SVG and PNG of the result. Each file is written whole and moved
into place, so a failed write never leaves half a picture.

## Permission

The folder must be one the operator allowed to be written to. The working
folder is read-only by default, so the first call asks (Allow once, Always allow
this folder, Deny); when the answer is no, or a graph or headless run cannot
ask, the call fails with one sentence naming the folder and the way to allow
it. Tell the user that sentence; do not retry.
