# Native pack eval suite

## Configuration

Use the bundled pack_config.json. The subject is the Engineer `gents init --engineer` makes at the commit under
test, every core pack installed as init installs them (`scripts/evals/suites.py`).

## Subject fixtures

- `native-pack-archives`: two built plugin pack archives,
  `artifacts/native_echo.pack` and `artifacts/native_authority.pack`, which a
  case copies into the workspace. A user hands the Engineer a pack archive
  to inspect and install.

## Usage

Run the opt-in native-packs eval suite through scripts/evals; artifacts are local and deterministic.
