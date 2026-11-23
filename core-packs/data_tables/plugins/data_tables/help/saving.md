`output` (a plain file name such as `result.csv`: no folders, not starting with a dot) writes the whole result of
`sql` into the folder `path` instead of returning rows; `path` must be a folder. Only a call with `output` writes,
and the user is asked to allow writing to that folder. `format` is `csv` or `parquet`, taken from the name when it
ends in `.csv` or `.parquet`; an existing file is replaced only when `overwrite` is true. The result is
`{"written": "result.csv", "format": "csv", "rows": 1250, "bytes": 48213, "warnings": []}`, and the file appears
only when it is complete. CSV keeps NULL (an empty field) apart from the empty string (`""`) and floats exact, so
reading it back gives the same rows; Parquet keeps every type. The new file is a table of the folder. Save a result
only when the user asks for a file.
