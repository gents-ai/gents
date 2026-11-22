# Proofs

## Lean owners

A Lean owner is the definition that makes one decision: a pure function from
observed state to an action, with the theorems about it. Models live in
`Proofs/`, one directory per crate layer, and import only downward. When a
model and the Rust disagree, the model is changed first and the Rust follows.

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

Exactly one Rust test replays each family against the production function,
with expectations taken only from the model:

```rust
gents_lean_vocab::families::<key>::replay(|input| production(input));
```

To find a family's consumer:

```bash
git grep -nw <key> -- crates apps packages ':!crates/gents-lean-vocab/families.schema.json'
```

## The scan

`crates/gents-lean-vocab/src/consumer_scan.rs` finds each family's consumer
from its key and refuses a family no test replays unless the coverage ledger
lists it, a ledger entry naming a consumed or unemitted family, a family
replayed by more than one test, and any new `*_unreached` family. One command
proves the layer closed (with the Makefile's `RUST_MIN_STACK`; read the test
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
- `openFamilies`: emitted families with no consumer, by name only. The list is
  being emptied: a family leaves it when its consumer is written or Lean stops
  emitting it, and `OPEN` in the scan pins it so it never grows.

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

Specs live in `tla/`. A spec names the Lean owners it reads on its second line
(`\* lean: Owner.decision`) and takes their tables from `tla/generated`. Each
configuration `MC_<Spec>_<Name>.cfg` states its expected result on its first
line (`\* expect: holds` or `\* expect: violates <Property>`). Run one with
`tla/scripts/run-tlc.sh MC_<Spec>_<Name>`, or compare results with the headers
with `python3 tla/scripts/tla_check.py run --config MC_<Spec>_<Name>`.
