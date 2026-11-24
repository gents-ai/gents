`(await tools.read_file({path})).content` is the file's text, so `JSON.parse` and `split('\n')` work on it. `truncated: true` means the file is longer than one read returns: read it in parts with `start_line` and `end_line`.

Write with `await tools.write_file({path, content})`; replacing an existing file needs `expected_content_hash`, the `content_hash` of your read. Reading and writing in the script is cheaper than pasting contents into `input`.
