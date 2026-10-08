# ux_fixture

A fixture `plugins` pack for UX plugin tests. It carries one UX plugin of
each production form the desktop loader accepts:

- `badge`: a plain ESM file (`ux/badge/plugin.js`) with a stylesheet
  beside it; the file door.
- `report`: produced by the pack's own `report_ui` plugin, whose stdout for
  the input `{"role": "ux", "surface": "webview"}` is
  `{"module": "<esm>", "css": "<css>"}`; the `.afb` door.

Used by `gents-cli`'s pack install, `gents pack check` lint and the desktop
bridge's `desktop_ux_plugin_source` tests.

## Configuration

No `pack_config.json`: a `plugins`-kind pack carries only its declared
plugins and UX plugins.
