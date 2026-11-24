# gents-schemas

This crate owns the agent-domain collection contract: the SDL in
`schemas/agent/`, exported from `src/lib.rs` as a `*_NAME` and `include_str!`
constant per type, and the access policies a home adds (`BuiltInPolicy` in
`src/policy_ids.rs`). `build.rs` reads this SDL and the protocol SDL in
`crates/gents-protocol/schemas/` and writes the field constants (`fields.rs`).
The crate has no dependencies, so a consumer that needs only the collection
contract depends on it instead of `gents-protocol`.

- SDL names a built-in policy by its placeholder (`$household`), never by ID;
  a home's `HomePolicyIds` fill the IDs in when the schema is loaded.
- `agent_ownership_policy.yaml` and `mailbox_policy.yaml` are generated from
  the Lean policy model by
  `cargo run -p gents-lean-vocab --example regenerate_household_policy`;
  regenerate them, never edit them.
