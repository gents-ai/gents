The engine's operators (sort, join, grouping) share 1 GiB: a query that needs more fails with a sentence saying
to narrow it with WHERE or LIMIT, select fewer columns, or aggregate first. Reading files is not counted there; it
streams, so a plain scan, filter or aggregate runs in flat memory whatever the file size, and the plugin as a
whole is stopped at its 3 GiB limit. SQL is limited to 64 KiB and 256 levels of nesting; a row is limited to
16 MiB. A call that fails exits with one sentence. Files that are not data are skipped and listed in the
`warnings` of `tables`; Arrow files are not supported. A workbook's strings are read only when one of its sheets
is named, and a workbook whose strings pass 512 MiB is refused with a sentence saying to save the sheet as CSV.
