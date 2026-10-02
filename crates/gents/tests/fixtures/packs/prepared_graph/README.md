# prepared_graph

A fixture `graph` pack for the entry prepare machinery: its one entry
declares an `input_schema` (a default and a pattern) and a `prepare` step
(`git_diff`, then the `prepare_fixture` plugin writing `FixtureEvidence`).
Test data, not a shipped pack.

## Plugin

`plugins/prepare_fixture` reads the host's `git_diff` fact and returns the job
input plus one `FixtureEvidence` document, both carrying the diff's head sha.
The `.afb` is not checked in: `gents pack build` compiles the source, and the
gents crate's tests write a constant-output module at the declared path in a
temp copy.

## Configuration

One behavior, `fixture-worker`, bound to the `worker` inference slot. The
capability scopes its allowed caller to `${GENTS_PACK_AGENT_DID}`.
## Declared topology

Compiled capability edges.

<!-- pack-topology:start -->
```mermaid
flowchart LR
    n0["worker"]

```
<!-- pack-topology:end -->
