# job_plugin_fixture

A fixture `documents` pack carrying one table-driven job plugin, `job_fixture`,
a `FixtureNote` schema and the `fixture_assets` asset class. Test data, not a
shipped pack.

The `.afb` is not checked in: `gents pack build` compiles the plugin source, and
the tests write a module at the declared path in a temp copy.

## Configuration

`pack_config.json` declares only an empty node. The schema is
`schemas/fixture_note.graphql`. The plugin `job_fixture` declares `job: true`,
`FixtureNote` query and insert data access with the stamped `job_row_doc_id` and
`chain_root_doc_id` fields, and the `fixture_assets` mount.

## Usage

Used by the plugin-job conformance and runtime tests. It is never installed onto
a runtime agent.
