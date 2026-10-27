# job_plugin_fixture

A fixture `documents` pack carrying one table-driven job plugin, `job_fixture`,
a `FixtureNote` schema and one plain asset, `fixture_assets/a.txt`. Test data, not a
shipped pack.

`plugins/job_fixture.afb` is built from `plugins/job_fixture/source` by `gents pack build` and checked in, and `plugins/job_fixture/Cargo.lock` pins the guest's dependencies so the build is reproducible.
The gents-cli test `committed_job_fixture_artifact_matches_its_source` fails when the artifact and its source drift, and `write_job_fixture_artifact` (ignored) regenerates it.
Some tests still write a module at the declared path in a temp copy.

## Configuration

`pack_config.json` declares only an empty node. The schema is
`schemas/fixture_note.graphql`. The plugin `job_fixture` declares `job: true`,
`FixtureNote` query and insert data access with the stamped `job_row_doc_id` and
`chain_root_doc_id` fields.

## Usage

Used by the plugin-job conformance and runtime tests.
The plugin-job end-to-end tests (`e2e_background`'s `plugin_job_*` modules and gents-cli's `cli_job` suite) install it onto a runtime agent.
