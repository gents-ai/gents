# TLA+ specs

A spec here models the interleaving of concurrent actors; every decision an
actor takes is a Lean owner the spec reads, never a restatement of its policy.
A spec `X.tla` describes what it models in the comment below its header, and
each configuration `MC*.cfg` (with its `MC*.tla` wrapper) is one checked
instance. The headers are the index: they name each spec's Lean owners and
crates and each configuration's expected result, and the scripts check them.

## Headers

A spec follows its `---- MODULE X ----` line with

```
\* lean: none | <Lean.owner>[, <Lean.owner>]...
\* crates: none | <crate>[, <crate>]...
```

and optionally, in this order,

```
\* models: <Lean.declaration>[, <Lean.declaration>]...
\* paths: <repository path>[, <repository path>]...
```

A `lean:` owner is a `@[conformance_function]` over a finite domain (its key is
in `crates/gents-lean-vocab/families.schema.json`); the spec `EXTENDS`
`generated/LeanDecision_<Namespace>.tla`, which holds its table, and defines no
operator of the same name. A `models:` declaration is Lean the spec restates by
hand, named so that changing it selects the spec. A `paths:` entry is a file or
directory outside every crate that the spec models.

A configuration's first line states the result TLC must report:

```
\* expect: holds
\* expect: deadlock
\* expect: violates <Name>
\* expect: violates <Name> initially
```

`<Name>` is one `INVARIANT` or `PROPERTY` the configuration checks; `initially`
records that the initial state violates it. `deadlock` requires deadlock
checking on. An optional second line `\* states: <N>` sets the fewest distinct
states TLC must find (default 2), so a model whose initial predicate admits
nothing, or that stops after its first state, does not pass.

## Configurations

Each spec has a configuration where its properties hold and, per safeguard the
product relies on, one that removes the safeguard and fails with the property
it names. A configuration that stops failing as its header says is a spec that
no longer catches its bug: fix the model or the header deliberately, never by
weakening a property.

## Commands

From the repository root:

```bash
python3 crates/gents/proofs/tla/scripts/tla_check.py check-headers
python3 crates/gents/proofs/tla/scripts/tla_check.py run --spec <Spec>
python3 crates/gents/proofs/tla/scripts/tla_check.py run --config <MC config>
python3 crates/gents/proofs/tla/scripts/tla_check.py select --files <path>...
crates/gents/proofs/tla/scripts/run-tlc.sh <MC config>
```

`check-headers` validates every header: each owner is in the snapshot, each
`models:` declaration and path exists, each crate is a workspace package, each
configuration has a header, and each `LeanDecision_*` module a spec extends
exists and holds one of its owners. `run` (also `--all`, `--crate`, `--lean`,
`--files`) model-checks breadth-first with one worker and a fixed fingerprint,
compares each result with its header and fails on a mismatch, too few states, a
missing header, a TLC error or a timeout. It records each outcome under a
digest of the bytes TLC reads in the user cache and reuses it for the same
digest; `--fresh` runs TLC anyway. `install` fetches the pinned JDK and
`tla2tools.jar`. `run-tlc.sh` prints TLC's full output, counterexample
included.

The gate runs the specs a change selects; the `TLA+ nightly` workflow runs
every configuration with `--fresh`.

## Generated decisions

`make lean-regen` writes `generated/LeanDecision_<Namespace>.tla` for every
`lean:` owner: `<name>_table`, the decision over its whole domain keyed by the
tuple of its arguments in Lean parameter order (a balanced tree of `@@`,
because a long left-nested chain overflows TLC's stack), and `<name>(...)`,
its lookup. These files are regenerated, never edited;
`generated_tla_modules_are_current` in `gents-lean-vocab` fails when one is
stale.
