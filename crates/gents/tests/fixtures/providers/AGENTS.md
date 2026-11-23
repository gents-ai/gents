# Provider wire fixtures

Provider request and response replay fixtures, one directory per provider (or
`synthetic/`) and one file per scenario. A fixture is recorded from a live
provider and redacted before it touches disk; refresh fixtures through an
operator run against the provider, never in pull-request CI.

Commit only redacted fixtures: no access or refresh tokens, API keys, account
ids, bearer values, or provider-specific credential query parameters.
`provider_wire_fixtures_do_not_contain_credentials` (in
`crates/gents/tests/e2e_runtime/provider_fixture_redaction.rs`) scans every
`.json`, `.jsonl`, `.http`, `.sse` and `.txt` file here for credential
patterns.
