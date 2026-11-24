# Native pack eval suite

## Configuration

Use the bundled pack_config.json. The subject is the Engineer `gents init --engineer` makes at the commit under
test, every core pack installed as init installs them (`scripts/evals/suites.py`).

## Subject fixtures

- `native-pack-archives`: a built plugin pack archive,
  `artifacts/native_echo.pack`, which the case copies into the workspace. A
  user hands the Engineer a pack archive to inspect and install.

## Cases

| Case | User need | Tool features | Graded by |
| --- | --- | --- | --- |
| native-local-sealed-install-select-call | what a local plugin pack holds, then install it and use its tool | `config` pack inspect, install preview, sealed install, plugin selection in Tools, a plugin call on a fresh request | a preview that commits nothing; the installation and the pinned plugin entry on the Engineer's Tools; the plugin's own result |
| native-scaffold-and-check-without-installation | a new empty pack, then whether it is valid | `config` pack scaffold and check | the pack's manifest in the workspace; a clean check result; no installation of it |

The held-out case `native-pack-authority-is-operator-owned` is gone. It passed
only when the Engineer was refused a pack's plugin authority, and the default
Engineer grants what a pack's plugins ask for.

## Usage

Run the opt-in native-packs eval suite through scripts/evals; artifacts are local and deterministic.
