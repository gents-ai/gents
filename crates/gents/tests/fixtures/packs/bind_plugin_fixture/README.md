# bind_plugin_fixture

A fixture `plugins` pack for `bind_dir` tests: its one plugin, `list_files`,
lists the file names directly under whatever directory its caller binds it
to. Used by `gents-cli`'s `gents config pack plugin run --bind-dir` and `gents config pack
test` bind-case tests.

## Configuration

No `pack_config.json`: a `plugins`-kind pack carries only its declared
plugin (`list_files`, source `plugins/list_files`) and no documents.

## Usage

Build it with `gents config pack build`, then bind a directory per call:

```
gents config pack plugin run fixture/list_files --bind-dir /some/directory
```
