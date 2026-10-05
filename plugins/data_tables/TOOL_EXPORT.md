# data_tables_export

Runs one SELECT over data files, like `data_tables`, and writes the whole result as a file in the folder you
name. Use it only when the user wants the result saved; to read a result, use `data_tables`.

The user is asked to allow writing to the folder the first time. `path` must be a folder (not one file), and it
is where the file lands.

## Input

One JSON object: `path` (the folder), `sql` (one SELECT), `output` (the file name, such as `result.csv`: a plain
name, no folders, not starting with a dot) and optionally `format` (`csv` or `parquet`; taken from the name
when it ends in `.csv` or `.parquet`), `overwrite` (default false: an existing file is never replaced unless
this is true), `files`, `tables`, `delimiter` and `header`, which mean what they do for `data_tables`.

## Output

`{"written": "result.csv", "format": "csv", "rows": 1250, "bytes": 48213, "warnings": []}`

The result is streamed to the file, whatever its size, and appears only when it is complete. CSV keeps NULL (an
empty field) apart from the empty string (`""`) and writes floats with their shortest exact digits, so reading
the file back gives the same rows; a one-column file cannot hold a NULL row, so NULLs there are written as empty
strings and `warnings` says how many. Parquet keeps every type. The new file is a table of the folder: query it
with `data_tables` as `result`.
