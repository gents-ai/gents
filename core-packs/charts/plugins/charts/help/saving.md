`save` writes the chart into the folder named in `path` (a folder, not a file;
the data is `file` inside it, or inline in `data`). A base name like
`"revenue"` writes `revenue.svg` and `revenue.png` at full size;
`{"svg": "a.svg", "png": "a.png"}` names them (either may be left out); a
subfolder like `"out/revenue"` is created. Names stay inside the folder and an
existing file is replaced. `response.files` lists what was written.

Writing needs the operator's permission for that folder: the first save may
ask, and a refusal is one sentence naming the folder. Tell the user that
sentence; do not retry the same save. Save only when the user asks to keep
the chart.
