# engineer_pages

## Configuration

Use the bundled pack_config.json. The subject is `../engineer_full` with every
core pack installed, so the Engineer holds `config` (pages, datastore
surfaces), `charts` and `data_tables` as `gents init --engineer` leaves them.

A case that repairs or edits a page starts with a `setup` stage in which the
Engineer writes the starting surface and Page documents itself, in one
dictated `config` batch: a Page or DatastoreToolSurface the evaluator writes
as a fixture is not the Engineer's to read or edit. Setup checks are
development tier; fixtures supply only the app collections and their rows.

Page cases grade the stored Page and DatastoreToolSurface documents and the
page lint; chart cases grade the chart in the charts call's output segment;
table cases grade the rows the SQL returned; every case also requires that no
workspace file changed.

## Usage

Run it through scripts/evals with `run-ladder.sh engineer-pages`; held-out
cases run only with `GENTS_EVAL_SPLITS=held_out`.
