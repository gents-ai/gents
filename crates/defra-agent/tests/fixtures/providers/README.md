# Provider Wire Fixtures

This directory is reserved for provider request/response replay fixtures (#545).

Fixtures are recorded from live providers, redacted before they touch disk, then
replayed offline in CI. They are the provider wire contract for future native
provider clients and rig-removal work.

Rules:

- Commit only redacted fixtures.
- Do not commit access tokens, refresh tokens, API keys, account ids, bearer
  values, or provider-specific credential query parameters.
- Keep provider fixtures versioned by provider and scenario.
- Refresh fixtures through a live/operator workflow, not normal PR CI.

The `provider_wire_fixtures_do_not_contain_credentials` test scans committed
fixture files in this directory for common credential patterns.
