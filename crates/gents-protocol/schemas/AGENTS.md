# Protocol schemas

The SDL for inference (`inference/`: backends, credentials, profiles,
sampling, execution and retry policy, inference calls) and tool services
(`services/`). The agent-domain collections live in `crates/gents-schemas`,
whose `build.rs` reads these files too; its rules apply here.

- `Agent` has one context selection (`AgentContext`: instructions, skills,
  tools, compaction) and one inference selection (`InferenceProfile`:
  backend, sampling, execution bounds, retries). Backend authentication is the
  tagged `InferenceBackend.auth` value, the only credential field.
- Configuration references are owner-scoped unless the target models a
  foreign principal (`AgentTarget.target_node_did`). Tags are discovery
  metadata, never references, permissions or execution selectors.
- Desired state publishes a complete owner graph atomically and validates
  every reference before commit.
- Host-local endpoints, credentials, filesystem placement, health and process
  observations stay local; only durable configuration and facts replicate.
