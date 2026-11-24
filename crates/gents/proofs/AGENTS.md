# Proofs

## Lean owners

A Lean owner is the definition that makes one decision: a pure function from
observed state to an action, with the theorems about it. Models live in
`Proofs/`, one directory per crate layer, and import only downward. When a
model and the Rust disagree, the model is changed first and the Rust follows.

## Building

`lake build` (in this directory, with the toolchain in `lean-toolchain`) builds
the `Proofs` library from `proofRoots` in `lakefile.lean` and runs the default
checks: `importClosure` refuses a module outside the roots' import closure,
`importLayers` refuses an import that points up the layers in
`.github/scripts/lean-import-layers.json` (which also records modules placed
away from their Rust owner, with the reason), and `canonicalOutputMap` checks
`contracts/canonical-output-map-*.json`. A module under `Proofs/` with its own
`main` is listed in `proofRoots`.

## Proof map

Each layer directory under `Proofs/` specifies one crate layer (the JSON above
names the crates); a model group keeps its path below the directory, so one
group can span layers.

| Directory | Models |
| --- | --- |
| `Shared` | Ids and time, provider-input shaping shared by `gents-protocol`, `gents-loop` and `gents-claude` (rendered capture, tool result text, account rotation, completion retry), canonical output state |
| `Store` | Store access and its errors (`AccessClassification`, `DecisionRead`, `StorageFailure`), persistence and storage observation, the write gate, home opening and handoff, key custody, subscriptions, and the access policy (`Identity/`; `cargo run -p gents-lean-vocab --example regenerate_household_policy` writes its YAML into `crates/gents-schemas/schemas/agent/` through `GenerateHouseholdPolicy.lean` and `GenerateMailboxPolicy.lean`) |
| `Documents` | Configuration documents and their publication (`ApplyReconcile/`), defaults, desired state, scope templates, skills, document views |
| `Inference` | Backend admission and inference calls, scheduling, accounts and their resolver, usage reads, inference recovery |
| `P2P` | Enrollment, pairing reconcile and routes, peer discovery, the backpressure obligation model |
| `Pack` | Pack resolution, core packs, plugin wiring, grants and ownership |
| `Session` | Requests and execution leases, sessions, forks and recovery, the session queue, prompt assembly, compaction and transcripts, command policy, managed execution, workspaces, hooks, client turn-state derivation (`Client/`) |
| `Tools` | The tool surface, self-config and its grants, tool policy, root admission, edit matching, LSP and MCP health |
| `ToolCalls` | Tool dispatch, background tool rows, plugin jobs, trigger identity, tool-call recovery |
| `Automation` | Triggers and event delivery, callbacks, Goals and Goal automation, the mailbox, processes, hook instantiations |
| `Observe` | File inventory, retry summaries |
| `Runtime` | The completion loop and its retry, canonical output execution, inference-call composition, runtime reconcile and startup, backend health, evals and optimization, the cross-machine composition and the request/process properties (`Properties/`) |
| `Clients` | The client shell (`ClientShell/`), observation and sync health, client identity metadata, outbox, installation repair, service lifecycle |
| `Conformance` | Family emitters, the contract program (`Contracts.lean`), the coverage ledger |

## What a theorem claims

Lean liveness is existential unless a theorem says otherwise: an
`*_eventually_*` or `*_convergence` name states that a finite legal path
exists, not that a fair scheduler takes it or that it finishes in bounded time.
Fair-scheduler progress belongs to TLA+, and deadlines and watchdogs to the
Rust owners. Time and the request, peer and node ids are abstract naturals
(`boundary.model.nat-typed-ids-time`). DefraDB storage, merge and event
delivery, the network, providers, external tools and the OS sandbox are
premises, each a `boundary.` entry in the coverage ledger.

## Conformance families

An owner becomes a family that Rust replays:

- `@[conformance_function]` marks a function over a finite domain; every input
  is a case.
- `@[conformance_family]` marks a `Conformance.Family` (`inputs`, `model`) when
  the domain is not finite; the inputs are chosen by hand.

A family's key is its declaration name in snake case with the components joined
by `_`; `crates/gents-lean-vocab/families.schema.json` lists every key with its
wire shape.

## Generated files

`make lean-regen` runs the model (`Proofs/Conformance/Contracts.lean`) and
writes `crates/gents-lean-vocab/families.schema.json` (from which the
`gents_lean_vocab::families::<key>` types are built),
`crates/gents-lean-vocab/src/generated.rs` (the snapshot decoders) and
`tla/generated/LeanDecision_*.tla` (the decision tables TLA+ specs read).
These files are regenerated, never edited. Tests load the snapshot from Lean
when they run.

## One consumer per family

Exactly one test that runs replays each family against the production
function, with expectations taken only from the model. A consumer counts only
in a Rust file some target compiles (reached from a target root through `mod`,
`#[path]` or `include!`) or in a TypeScript test; a file outside every module
tree is dead source, and the structure test `every_rust_source_is_compiled`
refuses a new one.

```rust
gents_lean_vocab::families::<key>::replay(|input| production(input));
```

To find a family's consumer:

```bash
git grep -nw <key> -- crates apps packages ':!crates/gents-lean-vocab/families.schema.json'
```

## The scan

`crates/gents-lean-vocab/src/consumer_scan.rs` finds each family's consumer
from its key in sources that compile and refuses a family no test replays
unless the coverage ledger lists it, a ledger entry naming a consumed or
unemitted family, a family replayed by more than one test, and any new
`*_unreached` family. One command proves the layer closed (with the Makefile's `RUST_MIN_STACK`; read the test
count, a filter matching nothing passes):

```bash
cargo test -p gents-lean-vocab consumer_scan
```

## The coverage ledger

`Proofs/Conformance/CoverageLedger.lean` lists only what no test consumes; a
consumed family has no entry.

- `boundaries`: a permanent fact about the database, operating system, host,
  provider or WASM guest that no Gents test observes, with its reason. A name
  beginning `boundary.` is a premise; any other name is an emitted family whose
  cases only that fact decides.
- `openFamilies`: emitted families with no consumer, by name only. It is
  empty, and `OPEN` in the scan pins it so it never grows: a new family
  arrives with its consumer.

An owner emits one family over the cases production reaches; cases production
cannot express stay Lean theorems, not an `*_unreached` complement family.
`featureSurfaceRequirements` names each surface a feature reaches and one
consumed family whose test evidences it.

## Adding a family

1. Write or change the owner; no `sorry`.
2. Mark it `@[conformance_function]` or `@[conformance_family]` and run
   `make lean-regen`.
3. In the same change, write the test that replays it against the production
   function.
4. Run the scan.

## TLA+

Specs live in `tla/`; [tla/AGENTS.md](tla/AGENTS.md) gives the header grammar,
the expected-result rule and the commands.
