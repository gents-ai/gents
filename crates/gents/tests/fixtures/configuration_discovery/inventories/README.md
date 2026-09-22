# Canonical configuration discovery fixtures

These JSON documents are the stable handoff surface for onboarding consumers.
They deserialize as `ConfigurationDiscoveryInventory` and are pinned byte-for-byte
to `ConfigurationDiscoveryInventory::to_model_json_pretty()` by unit tests.

- `complete-v1.json` is produced by scanning the checked-in conflicting Codex
  user/project source fixture. It includes untrusted instruction excerpts,
  redaction, a disabled remote tool, partial mappings, and unresolved conflicts.
- `partial-v1.json` is produced by scanning a malformed/oversized Claude source
  plus a missing Grok source. It demonstrates partial and unavailable outcomes,
  warnings, and machine-readable truncation.

Consumers should decode the canonical type/schema rather than infer compatibility
or precedence from these examples. The synthetic roots are normalized solely to
make the serialized fixtures portable.
