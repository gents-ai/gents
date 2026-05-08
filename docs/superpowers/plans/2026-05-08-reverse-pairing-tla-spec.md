# Reverse-Pairing TLA+ Spec — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the TLA+ specification defined in `docs/superpowers/specs/2026-05-08-reverse-pairing-tla-design.md` — an abstract model of reverse-pairing subscription/replicator convergence between two peers — and verify under TLC that structural safety, the in-flight supporting invariant, and leads-to liveness all hold under bounded model parameters.

**Architecture:** Single TLA+ module `ReversePairing.tla` containing the abstract model: state, RPC kinds, actions (operator-write + reconcile, send / deliver / drop, process, ack, timeout, crash, recover), and properties. A separate `MCReversePairing.tla` + `MCReversePairing.cfg` configure bounded model-checking parameters (2 nodes, 2 collections, ≤ 2 crashes per node). A wrapper script invokes TLC. Everything lives under `crates/defra-agent/proofs/tla/`, sibling to the existing Lean proofs at `crates/defra-agent/proofs/Proofs/`.

**Tech Stack:** TLA+ syntax (PlusCal not used; raw TLA+ throughout), TLC model checker (Java-based, distributed via `tla2tools.jar`), bash wrapper script.

---

## Decisions made (overridable before execution)

The spec left several formulation choices open. The plan commits to defaults below; each can be revisited mid-execution if it doesn't pan out.

1. **Tooling: TLC.** Bounded model-checking is sufficient for the parameters we care about. Apalache (symbolic, SMT-based) is more expressive but has a steeper on-ramp; defer until TLC hits a wall.
2. **Directory: `crates/defra-agent/proofs/tla/`.** Sibling to the existing Lean `Proofs/` directory under one verification root.
3. **OperatorWrite and Reconcile: separate actions.** `OperatorWrite(n, p, S)` atomically updates `desired` only — no RPCs emitted. `Reconcile(n)` emits Install/Teardown RPCs to bridge `desired ↔ replicator` gaps and may fire from any state, not only after operator action. This matches the spec's action signatures and supports crash recovery: a `Reconcile` fires after a `Crash` (when `inFlight` is cleared and any in-transit RPCs may have been dropped) to re-emit RPCs lost during the crash window. The supporting invariant from the spec is qualified to "states where Reconcile is not enabled" per the spec's supporting-invariant subsection. Phase-variable formulation (which would track the OperatorWrite/Reconcile window explicitly as state) deferred.
4. **`messages` is a set, not a multiset.** RPC IDs are unique per attempt, so set semantics suffice; duplicate-message scenarios remain modelable via `Send` re-emitting under different IDs. Saves complexity for first pass.
5. **Provenance via history variable: deferred.** Provenance is the right structural safety property but TLC's auxiliary-history support is awkward and inflates state. Cover later via TLAPS proof or post-hoc trace inspection.
6. **Bounded parameters: 2 nodes × 2 collections × ≤ 2 crashes per node.** Keeps state space tractable for laptop-grade TLC runs (target: < 5 minutes for safety; < 15 minutes for liveness).

---

## What's NOT in this plan (deferred)

- **Multi-node harness.** Spec §"Harness" describes the differential-conformance harness consuming TLA+ traces as JSON scenarios. Separate plan.
- **Issues for the four derived requirements** (persist-before-ack on receivers, idempotent handlers, Rust gossipsub-subscription persistence, stuck-retry visibility). Held until TLC validates the model; per the brainstorming session.
- **Lean idempotency lemmas** for derived requirement #2. Sibling Lean plan.
- **Provenance.** See decision 5 above.
- **Phase-variable formulation.** See decision 3 above.
- **N-peer fanout (N > 2), data-plane convergence, authorization correctness, multi-collection batched RPCs.** All explicit non-goals in the spec.

---

## Conventions

- **Run command:** `./scripts/run-tlc.sh MCReversePairing` from `crates/defra-agent/proofs/tla/`. Treat any output line containing `Error:`, `is violated`, `unsuccessful`, or any nonzero exit code as failure unless a step explicitly expects it (e.g., a step intentionally introducing a violating invariant to verify TLC catches it).
- **TDD in TLA+:** the "failing test" pattern is "introduce an invariant that is *expected* to hold; if TLC reports a violation, the model has a bug or the invariant is wrong; if TLC reports success, the invariant holds." For liveness, the analog is enabling fairness annotations and watching for `Temporal property check failed`.
- **Commit cadence:** one commit per task. Imperative, scoped commit messages with `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>` trailer.
- **Working directory:** all paths relative to repo root (`/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent-design-reverse-pairing-tla-spec`). Git working directory stays at repo root throughout.
- **Branch:** `design/reverse-pairing-tla-spec` (already current).
- **Iteration on TLC errors:** when TLC reports a counterexample trace, do NOT silently weaken the property to make the trace go away. Either fix the model (action's transition relation is wrong) or fix the property (overstated). Document which in the commit message.
- **TLA+ syntax conventions:** module names CamelCase; identifiers camelCase; constants ALL_CAPS; predicates end in `OK` or use noun-form (`TypeOK`, `Safety`). Indentation: 2 spaces, conjunctions/disjunctions stacked with `/\` and `\/` aligned at column 1 of the bullet level.

---

## File structure

Created files under `crates/defra-agent/proofs/tla/`:

```
ReversePairing.tla       # main model: types, state, actions, invariants, properties
MCReversePairing.tla     # MC instance: instantiates ReversePairing with bounded constants
MCReversePairing.cfg     # TLC config: SPECIFICATION, INVARIANTS, PROPERTIES, CONSTANTS
README.md                # how to run, expected output, parameter knobs
scripts/
  run-tlc.sh             # bash wrapper: invokes java -jar tla2tools.jar
  install-tools.sh       # downloads tla2tools.jar into .tools/
.tools/                  # gitignored: tla2tools.jar lives here
```

Modified files:
- `.gitignore` — ignore `crates/defra-agent/proofs/tla/.tools/` and `crates/defra-agent/proofs/tla/states/`
- `crates/defra-agent/proofs/README.md` — short pointer at the new `tla/` subtree

---

## Task 1: TLA+ tooling setup and project skeleton

Set up the directory, the TLC wrapper, and a sanity-check module to confirm the toolchain works end-to-end before writing any real model code.

**Files:**
- Create: `crates/defra-agent/proofs/tla/scripts/install-tools.sh`
- Create: `crates/defra-agent/proofs/tla/scripts/run-tlc.sh`
- Create: `crates/defra-agent/proofs/tla/Sanity.tla`
- Create: `crates/defra-agent/proofs/tla/Sanity.cfg`
- Create: `crates/defra-agent/proofs/tla/README.md`
- Modify: `.gitignore`
- Modify: `crates/defra-agent/proofs/README.md`

- [ ] **Step 1: Add TLA+ artifacts to `.gitignore`**

Append to repo root `.gitignore`:

```gitignore
# TLA+ tooling and TLC state output
crates/defra-agent/proofs/tla/.tools/
crates/defra-agent/proofs/tla/states/
```

- [ ] **Step 2: Create the directory structure**

```bash
mkdir -p crates/defra-agent/proofs/tla/scripts
mkdir -p crates/defra-agent/proofs/tla/.tools
```

- [ ] **Step 3: Write the install-tools script**

`crates/defra-agent/proofs/tla/scripts/install-tools.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION="${TLA_VERSION:-v1.8.0}"
URL="https://github.com/tlaplus/tlaplus/releases/download/${VERSION}/tla2tools.jar"
DEST=".tools/tla2tools.jar"

mkdir -p .tools
if [[ -f "$DEST" ]]; then
  echo "tla2tools.jar already present at $DEST"
else
  echo "Downloading tla2tools.jar ${VERSION}..."
  curl -fL "$URL" -o "$DEST"
fi

java -cp "$DEST" tlc2.TLC -h | head -1
```

Make executable:

```bash
chmod +x crates/defra-agent/proofs/tla/scripts/install-tools.sh
```

Run it to download the jar:

```bash
./crates/defra-agent/proofs/tla/scripts/install-tools.sh
```

Expected last line of output starts with: `TLC2 Version 2.18 of` (or similar — version date may vary).

- [ ] **Step 4: Write the run-tlc wrapper**

`crates/defra-agent/proofs/tla/scripts/run-tlc.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

JAR=".tools/tla2tools.jar"
MODULE="${1:?usage: run-tlc.sh <module> [extra TLC args...]}"
shift

if [[ ! -f "$JAR" ]]; then
  echo "Missing $JAR — run scripts/install-tools.sh first." >&2
  exit 1
fi

mkdir -p states
exec java -XX:+UseParallelGC -cp "$JAR" tlc2.TLC \
  -workers auto \
  -metadir states \
  "$@" \
  "$MODULE"
```

Make executable:

```bash
chmod +x crates/defra-agent/proofs/tla/scripts/run-tlc.sh
```

- [ ] **Step 5: Write the Sanity module**

A minimal counter to verify TLC can parse, build, and check a TLA+ module end-to-end.

`crates/defra-agent/proofs/tla/Sanity.tla`:

```tla
---- MODULE Sanity ----
EXTENDS Naturals

VARIABLE x

Init == x = 0

Next == x' = (x + 1) % 4

Spec == Init /\ [][Next]_x

Bounded == x \in 0..3

====
```

`crates/defra-agent/proofs/tla/Sanity.cfg`:

```
SPECIFICATION Spec
INVARIANT Bounded
```

- [ ] **Step 6: Run TLC against Sanity to verify the toolchain**

```bash
cd crates/defra-agent/proofs/tla && ./scripts/run-tlc.sh Sanity
```

Expected output ends with:

```
Model checking completed. No error has been found.
  Estimates of the probability that TLC did not check all reachable states
  ...
4 states generated, 4 distinct states found, 0 states left on queue.
```

If TLC reports `Java not found`, install Java 11+ (`brew install openjdk@17`).

If TLC reports a parse error in `Sanity.tla`, check that the `----` lines are present at top and bottom and that `EXTENDS Naturals` is on its own line.

- [ ] **Step 7: Write `crates/defra-agent/proofs/tla/README.md`**

```markdown
# TLA+ Specs

Distributed-system verification for cross-node properties. Sibling to `../Proofs/` (per-node Lean specs) under issue #155's cross-boundary verification strategy.

See `../README.md` for how this fits with the broader formal-verification model.

## Specs

- `ReversePairing` — control-plane convergence of subscription/replicator reverse-pairing between two peers. Spec design: `../../../../docs/superpowers/specs/2026-05-08-reverse-pairing-tla-design.md`.
- `Sanity` — toolchain smoke test; not a real model.

## One-time setup

```bash
./scripts/install-tools.sh
```

Downloads `tla2tools.jar` into `.tools/` (gitignored). Requires Java 11+ on `PATH`.

## Running a model-check

```bash
./scripts/run-tlc.sh MCReversePairing
```

The script runs TLC with parallel workers and writes state-graph artifacts to `states/` (gitignored).

## Bounded parameters

Current parameters in `MCReversePairing.cfg`:

- 2 nodes
- 2 collections
- ≤ 2 crashes per node

Edit the `CONSTANTS` block to change. Larger parameters increase state space exponentially; benchmark before raising them.

## Expected runtimes (2024 laptop)

- Safety check: < 5 minutes
- Liveness check (with `-deadlock` and fairness): < 15 minutes
```

- [ ] **Step 8: Update `crates/defra-agent/proofs/README.md`**

Find the section after the "What Is Proven" list (currently ending with the twelve numbered areas). Insert a new section before "Why This Matters":

```markdown
## Cross-node TLA+ specs

The `tla/` sibling directory contains TLA+ specifications for cross-node properties beyond per-node Lean coverage. See `tla/README.md`.

Currently:
- `ReversePairing` — control-plane convergence of reverse-pairing subscriptions; first concrete artifact under issue #155's cross-boundary verification strategy.
```

- [ ] **Step 9: Commit**

```bash
git add .gitignore \
        crates/defra-agent/proofs/README.md \
        crates/defra-agent/proofs/tla/Sanity.tla \
        crates/defra-agent/proofs/tla/Sanity.cfg \
        crates/defra-agent/proofs/tla/README.md \
        crates/defra-agent/proofs/tla/scripts/install-tools.sh \
        crates/defra-agent/proofs/tla/scripts/run-tlc.sh
git commit -m "$(cat <<'EOF'
Set up TLA+ tooling under proofs/tla/

Initial scaffolding for cross-node verification work tracking #155:
directory layout, install-tools and run-tlc wrapper scripts, sanity
module verifying TLC works end-to-end. tla2tools.jar lives in
gitignored .tools/.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Module skeleton — constants, state variables, type invariant

Define the abstract model's surface: what symbols stand in for nodes, collections, and RPC IDs; the persisted and in-memory state variables; and the type invariant `TypeOK` that constrains every variable to its declared shape. No actions yet — just shape.

**Files:**
- Create: `crates/defra-agent/proofs/tla/ReversePairing.tla`

- [ ] **Step 1: Write the module header and constants**

`crates/defra-agent/proofs/tla/ReversePairing.tla`:

```tla
---- MODULE ReversePairing ----
EXTENDS Naturals, FiniteSets, Sequences, TLC

(***************************************************************************)
(* Reverse-pairing subscription/replicator convergence between two peers.  *)
(* Spec design:                                                            *)
(*   docs/superpowers/specs/2026-05-08-reverse-pairing-tla-design.md       *)
(*                                                                         *)
(* This module models the abstract control-plane: state, RPC kinds, and    *)
(* actions. MCReversePairing.tla instantiates with bounded constants for   *)
(* TLC.                                                                    *)
(***************************************************************************)

CONSTANTS
  Node,        \* set of node identifiers (e.g., {"A", "B"})
  Collection,  \* set of collection identifiers (e.g., {"c1", "c2"})
  RPCId,       \* set of unique RPC identifiers; bounded for TLC
  MaxCrashes   \* per-node crash budget (Nat)

ASSUME NodeIsFiniteSet == IsFiniteSet(Node)
ASSUME CollectionIsFiniteSet == IsFiniteSet(Collection)
ASSUME RPCIdIsFiniteSet == IsFiniteSet(RPCId)
ASSUME MaxCrashesIsNat == MaxCrashes \in Nat

====
```

- [ ] **Step 2: Add an MC config to verify the skeleton parses**

Create temporary `crates/defra-agent/proofs/tla/MCReversePairing.tla`:

```tla
---- MODULE MCReversePairing ----
EXTENDS ReversePairing

CONSTANTS A, B, c1, c2, r1, r2

ASSUME NodeDef == Node = {A, B}
ASSUME CollectionDef == Collection = {c1, c2}
ASSUME RPCIdDef == RPCId = {r1, r2}
ASSUME MaxCrashesDef == MaxCrashes = 2

\* placeholder Init/Next so TLC has something to chew on
SkeletonInit == TRUE
SkeletonNext == UNCHANGED <<>>
SkeletonSpec == SkeletonInit /\ [][SkeletonNext]_<<>>

====
```

Create `crates/defra-agent/proofs/tla/MCReversePairing.cfg`:

```
CONSTANTS
  A = A
  B = B
  c1 = c1
  c2 = c2
  r1 = r1
  r2 = r2

SPECIFICATION SkeletonSpec
```

- [ ] **Step 3: Run TLC; verify it parses without error**

```bash
cd crates/defra-agent/proofs/tla && ./scripts/run-tlc.sh MCReversePairing
```

Expected: TLC parses, runs the trivial spec, reports `Model checking completed. No error has been found.` Exit 0.

If TLC reports `Parse Error` referencing `ReversePairing.tla`, check brackets and `====` line.

- [ ] **Step 4: Add state variables to `ReversePairing.tla`**

Inside `ReversePairing.tla`, after the `ASSUME` block:

```tla
VARIABLES
  desired,          \* desired[n][p] : SUBSET Collection — operator-set, persisted
  replicator,       \* replicator[n][p] : SUBSET Collection — n's local push-to-p replicator entries, persisted
  inFlight,         \* inFlight[n] : SUBSET RPC — caller's pending RPCs, in-memory
  pendingInbound,   \* pendingInbound[n] : SUBSET RPC — receiver's not-yet-processed RPCs, in-memory
  messages,         \* SUBSET RPC — in-transit network messages
  crashCount,       \* crashCount[n] : Nat — bookkeeping for the bounded crash budget
  rpcIdsUsed        \* SUBSET RPCId — IDs already issued, to enforce uniqueness
  
vars == <<desired, replicator, inFlight, pendingInbound, messages, crashCount, rpcIdsUsed>>
```

- [ ] **Step 5: Define the RPC structure**

Append to `ReversePairing.tla`:

```tla
(***************************************************************************)
(* RPC structure. Kind ∈ {"Install", "Teardown", "Ack"}. For Ack, `of`     *)
(* carries the originating RPC's id so the caller can match it.            *)
(***************************************************************************)

RPCKind == {"Install", "Teardown", "Ack"}

RPC == [
  id         : RPCId,
  kind       : RPCKind,
  src        : Node,
  tgt        : Node,
  collection : Collection,
  of         : RPCId \cup {NoOf}
]

NoOf == CHOOSE x : x \notin RPCId
```

Note: `of` is meaningful only for `Ack` RPCs; for `Install`/`Teardown` it's set to `NoOf` (a sentinel chosen disjoint from `RPCId`). This avoids modeling `Option` types in raw TLA+.

- [ ] **Step 6: Define `TypeOK`**

Append:

```tla
TypeOK ==
  /\ desired         \in [Node -> [Node -> SUBSET Collection]]
  /\ replicator      \in [Node -> [Node -> SUBSET Collection]]
  /\ inFlight        \in [Node -> SUBSET RPC]
  /\ pendingInbound  \in [Node -> SUBSET RPC]
  /\ messages        \in SUBSET RPC
  /\ crashCount      \in [Node -> 0..MaxCrashes]
  /\ rpcIdsUsed      \in SUBSET RPCId
```

- [ ] **Step 7: Define `Init`**

Append:

```tla
Init ==
  /\ desired        = [n \in Node |-> [p \in Node |-> {}]]
  /\ replicator     = [n \in Node |-> [p \in Node |-> {}]]
  /\ inFlight       = [n \in Node |-> {}]
  /\ pendingInbound = [n \in Node |-> {}]
  /\ messages       = {}
  /\ crashCount     = [n \in Node |-> 0]
  /\ rpcIdsUsed     = {}
```

- [ ] **Step 8: Add a placeholder `Next`**

Append, before the `====` close:

```tla
Next == UNCHANGED vars

Spec == Init /\ [][Next]_vars
```

- [ ] **Step 9: Update MC module to use real Spec, drop the skeleton**

Replace `MCReversePairing.tla` body with:

```tla
---- MODULE MCReversePairing ----
EXTENDS ReversePairing

\* Constants are bound via .cfg
====
```

Replace `MCReversePairing.cfg` body with:

```
CONSTANTS
  Node = {A, B}
  Collection = {c1, c2}
  RPCId = {r1, r2}
  MaxCrashes = 2

SPECIFICATION Spec
INVARIANT TypeOK
```

- [ ] **Step 10: Run TLC; verify `TypeOK` holds at `Init`**

```bash
cd crates/defra-agent/proofs/tla && ./scripts/run-tlc.sh MCReversePairing
```

Expected: `Model checking completed. No error has been found.` and `1 states generated, 1 distinct states found`. With `Next == UNCHANGED vars`, the only reachable state is `Init`; `TypeOK` holds there by construction.

- [ ] **Step 11: Commit**

```bash
git add crates/defra-agent/proofs/tla/ReversePairing.tla \
        crates/defra-agent/proofs/tla/MCReversePairing.tla \
        crates/defra-agent/proofs/tla/MCReversePairing.cfg
git commit -m "$(cat <<'EOF'
Add ReversePairing TLA+ module skeleton with TypeOK

State variables, RPC structure, Init, and a type invariant. Next is
a placeholder so TLC reaches a well-typed Init and exits clean. Real
actions land in subsequent commits.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: OperatorWrite and Reconcile (separate actions)

Per decision 3, operator intent and reconciliation fire independently. `OperatorWrite(n, p, S)` atomically updates `desired` only. `Reconcile(n)` emits one Install or Teardown RPC per firing to bridge a `desired ↔ replicator` gap and can fire from any state — including post-`Crash` recovery, when `inFlight` has been cleared and the persisted gap survives.

Single-RPC-per-firing keeps the action simple and tractable for TLC; multiple disagreements get reconciled across multiple firings, with fairness on `Reconcile` (added in Task 10) ensuring eventual progress.

**Files:**
- Modify: `crates/defra-agent/proofs/tla/ReversePairing.tla`

- [ ] **Step 1: Add helpers above any actions**

`Range` is defined before its first use to avoid a forward reference.

Append to `ReversePairing.tla` (above the placeholder `Next`):

```tla
(***************************************************************************)
(* Helpers                                                                 *)
(***************************************************************************)

Range(f) == { f[x] : x \in DOMAIN f }

FreshIds(k) ==
  \* True when there are at least k unused RPC ids available
  Cardinality(RPCId \ rpcIdsUsed) >= k

PendingInstallFor(n, p, c) ==
  \E rpc \in inFlight[n] :
    /\ rpc.kind = "Install"
    /\ rpc.tgt = p
    /\ rpc.collection = c

PendingTeardownFor(n, p, c) ==
  \E rpc \in inFlight[n] :
    /\ rpc.kind = "Teardown"
    /\ rpc.tgt = p
    /\ rpc.collection = c
```

- [ ] **Step 2: Add `OperatorWrite` action**

Append:

```tla
(***************************************************************************)
(* OperatorWrite(n, p, S): operator on node n sets desired[n][p] = S.      *)
(* Atomic update of desired only — no RPCs emitted. Reconcile fires        *)
(* separately to bridge any resulting gap.                                 *)
(*                                                                         *)
(* The S # desired[n][p] precondition prunes stutter steps where the       *)
(* operator writes the same value already present.                         *)
(***************************************************************************)

OperatorWrite(n, p, S) ==
  /\ p # n
  /\ S # desired[n][p]
  /\ desired' = [desired EXCEPT ![n] = [@ EXCEPT ![p] = S]]
  /\ UNCHANGED <<replicator, inFlight, pendingInbound, messages, crashCount, rpcIdsUsed>>
```

- [ ] **Step 3: Add `Reconcile` action**

Append:

```tla
(***************************************************************************)
(* Reconcile(n): emit ONE Install or Teardown RPC for some (p, c) pair    *)
(* where desired[n][p] and replicator[p][n] disagree, provided no matching *)
(* RPC is already in flight. Fires from any state (including post-Crash    *)
(* recovery, when inFlight has been cleared but the persisted gap          *)
(* survives).                                                              *)
(*                                                                         *)
(* Per-firing scope is one (p, c); multiple disagreements get reconciled   *)
(* across multiple firings under fairness.                                 *)
(***************************************************************************)

ReconcileInstall(n, p, c) ==
  /\ p # n
  /\ c \in desired[n][p]
  /\ c \notin replicator[p][n]
  /\ ~PendingInstallFor(n, p, c)
  /\ FreshIds(1)
  /\ LET id == CHOOSE i \in RPCId \ rpcIdsUsed : TRUE
         rpc == [id |-> id, kind |-> "Install", src |-> n, tgt |-> p,
                 collection |-> c, of |-> NoOf]
     IN /\ inFlight'    = [inFlight EXCEPT ![n] = @ \cup {rpc}]
        /\ messages'    = messages \cup {rpc}
        /\ rpcIdsUsed'  = rpcIdsUsed \cup {id}
  /\ UNCHANGED <<desired, replicator, pendingInbound, crashCount>>

ReconcileTeardown(n, p, c) ==
  /\ p # n
  /\ c \in replicator[p][n]
  /\ c \notin desired[n][p]
  /\ ~PendingTeardownFor(n, p, c)
  /\ FreshIds(1)
  /\ LET id == CHOOSE i \in RPCId \ rpcIdsUsed : TRUE
         rpc == [id |-> id, kind |-> "Teardown", src |-> n, tgt |-> p,
                 collection |-> c, of |-> NoOf]
     IN /\ inFlight'    = [inFlight EXCEPT ![n] = @ \cup {rpc}]
        /\ messages'    = messages \cup {rpc}
        /\ rpcIdsUsed'  = rpcIdsUsed \cup {id}
  /\ UNCHANGED <<desired, replicator, pendingInbound, crashCount>>

Reconcile(n) ==
  \/ \E p \in Node, c \in Collection : ReconcileInstall(n, p, c)
  \/ \E p \in Node, c \in Collection : ReconcileTeardown(n, p, c)
```

- [ ] **Step 4: Replace placeholder `Next` with the two actions**

Replace `Next == UNCHANGED vars`:

```tla
Next ==
  \/ \E n \in Node, p \in Node, S \in SUBSET Collection : OperatorWrite(n, p, S)
  \/ \E n \in Node : Reconcile(n)
```

- [ ] **Step 5: Disable deadlock check in MC config**

`OperatorWrite` and `Reconcile` may both eventually become disabled (operator stops writing, no remaining gaps, or `RPCId` pool exhausted). TLC would otherwise flag deadlock. Edit `MCReversePairing.cfg`, append:

```
CHECK_DEADLOCK FALSE
```

- [ ] **Step 6: Run TLC; verify TypeOK across the new state space**

```bash
cd crates/defra-agent/proofs/tla && ./scripts/run-tlc.sh MCReversePairing
```

Expected:
- TLC explores via `OperatorWrite` and `Reconcile`, including the OperatorWrite/Reconcile window where `desired` has changed but no RPC has been emitted yet
- `TypeOK` holds in every state
- State space is finite, bounded by `RPCId` pool exhaustion
- `Model checking completed. No error has been found.`

If TLC reports `RPCId pool exhausted` (deadlock-style stuck state), that's the bounded-model artifact and is fine — `CHECK_DEADLOCK FALSE` suppresses the report.

- [ ] **Step 7: Commit**

```bash
git add crates/defra-agent/proofs/tla/ReversePairing.tla \
        crates/defra-agent/proofs/tla/MCReversePairing.cfg
git commit -m "$(cat <<'EOF'
Add OperatorWrite and Reconcile as separate actions

Per spec decision 3, operator-write and reconcile fire independently.
OperatorWrite atomically updates desired only (with stutter-write
pruning). Reconcile emits one Install or Teardown RPC for a single
(p, c) gap, can fire from any state including post-Crash recovery.
Helpers (Range, FreshIds, PendingInstallFor, PendingTeardownFor)
defined before use.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Network actions — Deliver and Drop

The model treats `messages` as an in-transit set. `Send` already happens inside `Reconcile` (and later inside `Process` for acks); separate actions handle delivery and loss.

**Files:**
- Modify: `crates/defra-agent/proofs/tla/ReversePairing.tla`

- [ ] **Step 1: Add `Deliver` action**

Append (above `Next`):

```tla
(***************************************************************************)
(* Deliver(rpc): network delivers an in-transit message to its destination *)
(* node's pendingInbound queue.                                            *)
(***************************************************************************)

Deliver(rpc) ==
  /\ rpc \in messages
  /\ messages' = messages \ {rpc}
  /\ pendingInbound' = [pendingInbound EXCEPT ![rpc.tgt] = @ \cup {rpc}]
  /\ UNCHANGED <<desired, replicator, inFlight, crashCount, rpcIdsUsed>>
```

- [ ] **Step 2: Add `Drop` action**

Append:

```tla
(***************************************************************************)
(* Drop(rpc): network loses an in-transit message. Bounded by fairness so  *)
(* infinitely many drops do not occur in any execution; see liveness task. *)
(***************************************************************************)

Drop(rpc) ==
  /\ rpc \in messages
  /\ messages' = messages \ {rpc}
  /\ UNCHANGED <<desired, replicator, inFlight, pendingInbound, crashCount, rpcIdsUsed>>
```

- [ ] **Step 3: Update `Next` to include both**

Replace the `Next` definition:

```tla
Next ==
  \/ \E n \in Node, p \in Node, S \in SUBSET Collection : OperatorWrite(n, p, S)
  \/ \E n \in Node : Reconcile(n)
  \/ \E rpc \in messages : Deliver(rpc)
  \/ \E rpc \in messages : Drop(rpc)
```

- [ ] **Step 4: Run TLC; verify TypeOK still holds across the larger state space**

```bash
cd crates/defra-agent/proofs/tla && ./scripts/run-tlc.sh MCReversePairing
```

Expected: clean. State count grows because `Deliver` and `Drop` introduce new branchings. Should still complete in seconds.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/proofs/tla/ReversePairing.tla
git commit -m "$(cat <<'EOF'
Add network Deliver and Drop actions

Deliver moves an in-transit RPC from the global messages set into
the destination node's pendingInbound queue. Drop loses it. Drops
will be bounded by fairness annotations in the liveness task.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Process action with persist-before-ack

`Process` is the receiver-side handler. For `Install` it adds the requested collection to `replicator[recv][rpc.src]`; for `Teardown` it removes. In both cases, an `Ack` RPC is enqueued to `messages` after the persisted change is applied — atomically in the model, capturing the "persist before ack" derived requirement.

**Files:**
- Modify: `crates/defra-agent/proofs/tla/ReversePairing.tla`

- [ ] **Step 1: Add `Process` action**

Append (above `Next`):

```tla
(***************************************************************************)
(* Process(recv, rpc): receiver runs its handler.                          *)
(*   Install:  replicator[recv][rpc.src] gains rpc.collection.             *)
(*   Teardown: replicator[recv][rpc.src] loses rpc.collection.             *)
(* In both cases an Ack RPC is enqueued to messages atomically with the    *)
(* persisted change (modeling the persist-before-ack derived requirement). *)
(*                                                                         *)
(* Idempotent: Install for a collection already present is a state no-op   *)
(* (still emits an ack). Symmetric for Teardown. (Decision: model handlers *)
(* as inherently idempotent rather than parameterizing.)                   *)
(***************************************************************************)

ackOf(rpc) ==
  LET ackId == CHOOSE id \in RPCId \ rpcIdsUsed : TRUE  \* fresh id for the ack
  IN [ id         |-> ackId,
       kind       |-> "Ack",
       src        |-> rpc.tgt,
       tgt        |-> rpc.src,
       collection |-> rpc.collection,
       of         |-> rpc.id ]

Process(recv, rpc) ==
  /\ rpc \in pendingInbound[recv]
  /\ rpc.tgt = recv
  /\ rpc.kind \in {"Install", "Teardown"}
  /\ FreshIds(1)                                          \* ack needs an id
  /\ pendingInbound' = [pendingInbound EXCEPT ![recv] = @ \ {rpc}]
  /\ \/ /\ rpc.kind = "Install"
        /\ replicator' =
             [replicator EXCEPT ![recv] = [@ EXCEPT ![rpc.src] = @ \cup {rpc.collection}]]
     \/ /\ rpc.kind = "Teardown"
        /\ replicator' =
             [replicator EXCEPT ![recv] = [@ EXCEPT ![rpc.src] = @ \ {rpc.collection}]]
  /\ LET ack == ackOf(rpc) IN
       /\ messages'    = messages \cup {ack}
       /\ rpcIdsUsed'  = rpcIdsUsed \cup {ack.id}
  /\ UNCHANGED <<desired, inFlight, crashCount>>
```

- [ ] **Step 2: Update `Next`**

```tla
Next ==
  \/ \E n \in Node, p \in Node, S \in SUBSET Collection : OperatorWrite(n, p, S)
  \/ \E n \in Node : Reconcile(n)
  \/ \E rpc \in messages : Deliver(rpc)
  \/ \E rpc \in messages : Drop(rpc)
  \/ \E recv \in Node, rpc \in pendingInbound[recv] : Process(recv, rpc)
```

- [ ] **Step 3: Run TLC; observe replicator changes propagating**

```bash
cd crates/defra-agent/proofs/tla && ./scripts/run-tlc.sh MCReversePairing
```

Expected: more states, clean TypeOK. Run takes longer (still well under a minute).

If TLC reports an error from `CHOOSE id \in RPCId \ rpcIdsUsed : TRUE` because the set is empty, the model has run out of RPC ids. Increase `RPCId` cardinality in the .cfg (see step 4).

- [ ] **Step 4: If RPC id exhaustion was hit, expand `RPCId`**

Edit `MCReversePairing.cfg`:

```
CONSTANTS
  Node = {A, B}
  Collection = {c1, c2}
  RPCId = {r1, r2, r3, r4, r5, r6, r7, r8, r9, r10, r11, r12, r13, r14, r15, r16}
  MaxCrashes = 2
```

Sixteen RPC ids cover: up to 4 distinct (n, p, c) tuples × 2 directions (Install + Teardown) × 2 (original + ack) = 16 base, plus headroom for crash-driven re-emission. Bump higher (24+) if liveness check still reports id-pool exhaustion under fairness; lower if state space is too large.

Re-run TLC:

```bash
cd crates/defra-agent/proofs/tla && ./scripts/run-tlc.sh MCReversePairing
```

Expected: clean finish.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/proofs/tla/ReversePairing.tla \
        crates/defra-agent/proofs/tla/MCReversePairing.cfg
git commit -m "$(cat <<'EOF'
Add Process action with persist-before-ack semantics

Receiver-side handler updates replicator[recv][src] and atomically
enqueues an Ack to messages. Idempotent on duplicate Install/Teardown.
Models the persist-before-ack derived requirement as an inseparable
state transition: persisted change and ack emission are one atomic
TLA+ step, so receiver-side crash mid-handler is impossible by
construction in the abstract model.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: ReceiveAck and Timeout

`ReceiveAck` removes an RPC from the caller's `inFlight` once the matching `Ack` has been delivered. `Timeout` removes it without an ack — caller will re-issue on the next `Reconcile(n)` firing once `~PendingInstallFor(n, p, c)` (or teardown) holds again, since the corresponding RPC is no longer in `inFlight[n]`.

**Files:**
- Modify: `crates/defra-agent/proofs/tla/ReversePairing.tla`

- [ ] **Step 1: Add `ReceiveAck` action**

Append:

```tla
(***************************************************************************)
(* ReceiveAck(n, ack): caller matches an Ack from pendingInbound to an     *)
(* in_flight entry by `of`. Removes both. No persisted state change on n   *)
(* — the install/teardown happened on the peer's side.                     *)
(***************************************************************************)

ReceiveAck(n, ack) ==
  /\ ack \in pendingInbound[n]
  /\ ack.kind = "Ack"
  /\ ack.tgt = n
  /\ \E rpc \in inFlight[n] : rpc.id = ack.of
  /\ pendingInbound' = [pendingInbound EXCEPT ![n] = @ \ {ack}]
  /\ inFlight' =
       [inFlight EXCEPT ![n] = { rpc \in @ : rpc.id # ack.of }]
  /\ UNCHANGED <<desired, replicator, messages, crashCount, rpcIdsUsed>>
```

- [ ] **Step 2: Add `Timeout` action**

Append:

```tla
(***************************************************************************)
(* Timeout(n, rpc): caller drops an in_flight RPC without seeing an ack.   *)
(* Models the request-response timeout from the comm_channel pattern.      *)
(* Per spec §"Boundary discipline: timeouts" this is a liveness-only       *)
(* action: no other state changes.                                         *)
(***************************************************************************)

Timeout(n, rpc) ==
  /\ rpc \in inFlight[n]
  /\ inFlight' = [inFlight EXCEPT ![n] = @ \ {rpc}]
  /\ UNCHANGED <<desired, replicator, pendingInbound, messages, crashCount, rpcIdsUsed>>
```

- [ ] **Step 3: Update `Next`**

```tla
Next ==
  \/ \E n \in Node, p \in Node, S \in SUBSET Collection : OperatorWrite(n, p, S)
  \/ \E n \in Node : Reconcile(n)
  \/ \E rpc \in messages : Deliver(rpc)
  \/ \E rpc \in messages : Drop(rpc)
  \/ \E recv \in Node, rpc \in pendingInbound[recv] : Process(recv, rpc)
  \/ \E n \in Node, ack \in pendingInbound[n] : ReceiveAck(n, ack)
  \/ \E n \in Node, rpc \in inFlight[n] : Timeout(n, rpc)
```

- [ ] **Step 4: Run TLC; verify TypeOK across larger state space**

```bash
cd crates/defra-agent/proofs/tla && ./scripts/run-tlc.sh MCReversePairing
```

Expected: clean. State count grows further but should remain tractable (target: < 60s).

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/proofs/tla/ReversePairing.tla
git commit -m "$(cat <<'EOF'
Add ReceiveAck and Timeout actions

ReceiveAck matches an incoming Ack to its in_flight RPC by id and
removes both. Timeout drops an in_flight RPC; no other state change
(liveness-only per the spec's timeout discipline).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Crash and Recover

`Crash(n)` clears `n`'s in-memory state (`inFlight`, `pendingInbound`) and increments its crash count. Persisted state (`desired`, `replicator`) is preserved. Recovery is implicit via `Reconcile`: after `Crash` clears `inFlight[n]`, the next `Reconcile(n)` firing re-emits any RPCs needed to bridge the surviving `desired ↔ replicator` gap — including for messages that were `Drop`'d during the crash window. No explicit `Recover` step is needed because `Reconcile` fires from any state; fairness on `Reconcile` (Task 10) guarantees recovery completes. Crashes are bounded by `MaxCrashes` to keep the state space finite.

**Files:**
- Modify: `crates/defra-agent/proofs/tla/ReversePairing.tla`

- [ ] **Step 1: Add `Crash` action**

Append:

```tla
(***************************************************************************)
(* Crash(n): clears n's in-memory state (inFlight, pendingInbound) and    *)
(* increments crashCount. Bounded by MaxCrashes for finite model checking. *)
(* Persisted state (desired, replicator) survives.                         *)
(***************************************************************************)

Crash(n) ==
  /\ crashCount[n] < MaxCrashes
  /\ inFlight'       = [inFlight       EXCEPT ![n] = {}]
  /\ pendingInbound' = [pendingInbound EXCEPT ![n] = {}]
  /\ crashCount'     = [crashCount     EXCEPT ![n] = @ + 1]
  /\ UNCHANGED <<desired, replicator, messages, rpcIdsUsed>>
```

- [ ] **Step 2: Update `Next`**

```tla
Next ==
  \/ \E n \in Node, p \in Node, S \in SUBSET Collection : OperatorWrite(n, p, S)
  \/ \E n \in Node : Reconcile(n)
  \/ \E rpc \in messages : Deliver(rpc)
  \/ \E rpc \in messages : Drop(rpc)
  \/ \E recv \in Node, rpc \in pendingInbound[recv] : Process(recv, rpc)
  \/ \E n \in Node, ack \in pendingInbound[n] : ReceiveAck(n, ack)
  \/ \E n \in Node, rpc \in inFlight[n] : Timeout(n, rpc)
  \/ \E n \in Node : Crash(n)
```

(`Recover` is implicit: after `Crash`, the next `Reconcile(n)` re-emits any RPCs needed to bridge the surviving `desired ↔ replicator` gap. No explicit `Recover` step is needed because `Reconcile` is the recovery path.)

- [ ] **Step 3: Run TLC; expect a noticeably larger state space**

```bash
cd crates/defra-agent/proofs/tla && ./scripts/run-tlc.sh MCReversePairing
```

Expected: clean, target < 3 minutes. If runtime exceeds 5 minutes, lower `MaxCrashes` to 1 in the .cfg as a stopgap.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/tla/ReversePairing.tla
git commit -m "$(cat <<'EOF'
Add Crash action with bounded crash budget

Clears in-memory state on the crashed node (inFlight, pendingInbound)
and increments a per-node crash counter capped at MaxCrashes. desired
and replicator survive. Recovery is implicit via Reconcile: the next
Reconcile(n) firing re-emits any RPCs needed to bridge the surviving
desired/replicator gap, including any Drop'd during the crash window.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Structural safety invariants

Per the spec's safety section: structural invariants assert that state changes are mediated only by the documented actions. Each invariant is a property TLC can check on every reachable state.

**Files:**
- Modify: `crates/defra-agent/proofs/tla/ReversePairing.tla`
- Modify: `crates/defra-agent/proofs/tla/MCReversePairing.cfg`

- [ ] **Step 1: Define the structural invariants**

Append to `ReversePairing.tla` (above `Spec`):

```tla
(***************************************************************************)
(* Structural safety invariants                                            *)
(***************************************************************************)

(* Every RPC in messages or pendingInbound or inFlight has a unique id and *)
(* that id appears in rpcIdsUsed.                                          *)
RPCIdsTracked ==
  /\ \A rpc \in messages : rpc.id \in rpcIdsUsed
  /\ \A n \in Node, rpc \in inFlight[n] : rpc.id \in rpcIdsUsed
  /\ \A n \in Node, rpc \in pendingInbound[n] : rpc.id \in rpcIdsUsed

(* Every Install or Teardown RPC has src # tgt, kind in the right set, and *)
(* of = NoOf. Every Ack has of \in rpcIdsUsed.                            *)
RPCWellFormed ==
  \A rpc \in messages \cup
             UNION { inFlight[n] : n \in Node } \cup
             UNION { pendingInbound[n] : n \in Node } :
    /\ rpc.src # rpc.tgt
    /\ rpc.kind \in RPCKind
    /\ \/ /\ rpc.kind \in {"Install", "Teardown"}
          /\ rpc.of = NoOf
       \/ /\ rpc.kind = "Ack"
          /\ rpc.of \in rpcIdsUsed
```

- [ ] **Step 2: Add invariants to MC config**

Edit `MCReversePairing.cfg`:

```
CONSTANTS
  Node = {A, B}
  Collection = {c1, c2}
  RPCId = {r1, r2, r3, r4, r5, r6, r7, r8}
  MaxCrashes = 2

SPECIFICATION Spec
INVARIANT TypeOK
INVARIANT RPCIdsTracked
INVARIANT RPCWellFormed
CHECK_DEADLOCK FALSE
```

- [ ] **Step 3: Run TLC; verify all three invariants hold**

```bash
cd crates/defra-agent/proofs/tla && ./scripts/run-tlc.sh MCReversePairing
```

Expected: `Model checking completed. No error has been found.` All three invariants reported as checked.

If TLC reports `Invariant RPCIdsTracked is violated`: a code path emits an RPC without registering its id in `rpcIdsUsed`. Inspect the trace TLC prints; the offending action is the one named in the second-to-last state transition.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/tla/ReversePairing.tla \
        crates/defra-agent/proofs/tla/MCReversePairing.cfg
git commit -m "$(cat <<'EOF'
Add structural safety invariants

RPCIdsTracked: every RPC anywhere in the system has its id registered
in rpcIdsUsed. RPCWellFormed: src/tgt distinct, kind in the right set,
of-field correctly populated per kind. Both invariants checked by TLC
across all reachable states.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: Supporting invariant — in-flight justification (qualified)

Per the spec's "Supporting invariant: in-flight justification" subsection: every reachable state has the property that a disagreement between `desired[n][p]` and `replicator[p][n]` is *either* matched by a reconciling RPC somewhere in the system, *or* `Reconcile` is enabled to emit one. The disjunctive form qualifies the invariant for the OperatorWrite/Reconcile window — a state reached immediately after `OperatorWrite` (before `Reconcile` has fired) where the disagreement exists but no RPC has been emitted yet.

**Files:**
- Modify: `crates/defra-agent/proofs/tla/ReversePairing.tla`
- Modify: `crates/defra-agent/proofs/tla/MCReversePairing.cfg`

- [ ] **Step 1: Add the supporting invariant**

Append to `ReversePairing.tla` (above `Spec`):

```tla
(***************************************************************************)
(* Supporting invariant: in-flight justification.                          *)
(*                                                                         *)
(* For every reachable state, every desired/replicator disagreement is     *)
(* either matched by a reconciling RPC anywhere in the system (inFlight,   *)
(* messages, or pendingInbound), OR ReconcileInstall/ReconcileTeardown is  *)
(* enabled — meaning the system is one Reconcile firing away from emitting *)
(* the resolving RPC. The second disjunct qualifies the invariant for the  *)
(* OperatorWrite/Reconcile window and for post-Crash states where the gap  *)
(* exists but no RPC is in flight yet.                                     *)
(*                                                                         *)
(* This is the inductive invariant supporting the leads-to liveness        *)
(* property; under fairness on Reconcile, the second disjunct collapses    *)
(* to the first within finitely many steps.                                *)
(*                                                                         *)
(* Note: in pool-exhausted states (where ~FreshIds(1)) the second disjunct *)
(* fails. If TLC reports a violation only in exhausted-pool states, that's *)
(* a TLC-bounding artifact, not a model bug — bump RPCId in the .cfg.      *)
(***************************************************************************)

InstallJustified ==
  \A n, p \in Node, c \in Collection :
    (n # p /\ c \in desired[n][p] /\ c \notin replicator[p][n])
    => \/ \E rpc \in inFlight[n] \cup messages \cup pendingInbound[p] :
            /\ rpc.kind = "Install"
            /\ rpc.src = n
            /\ rpc.tgt = p
            /\ rpc.collection = c
       \/ /\ ~PendingInstallFor(n, p, c)
          /\ FreshIds(1)

TeardownJustified ==
  \A n, p \in Node, c \in Collection :
    (n # p /\ c \in replicator[p][n] /\ c \notin desired[n][p])
    => \/ \E rpc \in inFlight[n] \cup messages \cup pendingInbound[p] :
            /\ rpc.kind = "Teardown"
            /\ rpc.src = n
            /\ rpc.tgt = p
            /\ rpc.collection = c
       \/ /\ ~PendingTeardownFor(n, p, c)
          /\ FreshIds(1)

InFlightJustified == InstallJustified /\ TeardownJustified
```

- [ ] **Step 2: Add to MC config**

Edit `MCReversePairing.cfg` to add another `INVARIANT` line:

```
INVARIANT InFlightJustified
```

- [ ] **Step 3: Run TLC; expect the invariant to hold**

```bash
cd crates/defra-agent/proofs/tla && ./scripts/run-tlc.sh MCReversePairing
```

Expected: clean, all invariants hold including `InFlightJustified`.

- [ ] **Step 4: If TLC reports `InFlightJustified is violated`, inspect the trace**

The most likely failure modes:
- **Pool-exhausted state.** `~FreshIds(1)` AND no RPC anywhere AND a disagreement persists. This is a bounded-model artifact: TLC explored to id-pool exhaustion. Bump `RPCId` in `.cfg`.
- **`Process` atomicity.** `Process` should atomically update `replicator` and emit the ack. If TLC reports a violation at a state where `Process` has just fired, re-examine the action's `UNCHANGED` clauses to ensure the persisted change and ack-emission are in the same conjunctive step.
- **Crash leaving messages but clearing inFlight.** `Crash` clears `inFlight` and `pendingInbound` while `messages` may still hold the RPC. The invariant should still be satisfied via the `messages` disjunct (case 1) or via `Reconcile` enablement (case 2). If neither, the `Crash` action is over-clearing.

In all cases: fix the model, do not weaken the invariant. The invariant is the load-bearing claim that the model can always make progress toward convergence.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/proofs/tla/ReversePairing.tla \
        crates/defra-agent/proofs/tla/MCReversePairing.cfg
git commit -m "$(cat <<'EOF'
Add supporting invariant: in-flight justification (qualified)

InFlightJustified asserts that for every desired/replicator
disagreement, either a reconciling RPC exists somewhere in the
system, OR Reconcile is enabled to emit one. The disjunctive form
qualifies the invariant for the OperatorWrite/Reconcile window and
for post-Crash recovery states. Inductive invariant supporting the
leads-to liveness property to be added next.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: Fairness annotations and leads-to liveness

Per the spec's liveness section: under fairness on `Deliver`, `Process`, `ReceiveAck`, and `Reconcile`, every disagreement leads-to convergence. TLC checks liveness using the temporal property syntax with `~>` (leads-to).

**Files:**
- Modify: `crates/defra-agent/proofs/tla/ReversePairing.tla`
- Modify: `crates/defra-agent/proofs/tla/MCReversePairing.cfg`

- [ ] **Step 1: Add fairness to the spec definition**

Edit the `Spec` line in `ReversePairing.tla`:

```tla
Fairness ==
  /\ WF_vars(\E rpc \in messages : Deliver(rpc))
  /\ WF_vars(\E recv \in Node, rpc \in pendingInbound[recv] : Process(recv, rpc))
  /\ WF_vars(\E n \in Node, ack \in pendingInbound[n] : ReceiveAck(n, ack))
  /\ \A n \in Node : WF_vars(Reconcile(n))

Spec == Init /\ [][Next]_vars /\ Fairness
```

Notes on the fairness formulation:
- `WF_vars(\E x : Action(x))` — the standard idiom for parameterized actions: "eventually some Action fires" rather than the over-strong "every Action(x) for every x is fair." Quantifier scope inside `WF_vars` (not outside) is what's required.
- `\A n \in Node : WF_vars(Reconcile(n))` — per-node fairness on `Reconcile` is sufficient because each node's reconcile handles its own outbound RPCs; per-(p, c) granularity isn't needed.
- Weak fairness on `Drop`, `Timeout`, `Crash`, and `OperatorWrite` is NOT enabled — those are voluntary network/operator/crash actions that the model should be allowed to skip.
- `Reconcile` fairness is essential: without it, post-`Crash` recovery scenarios (where in-flight is cleared and any in-transit RPCs were `Drop`'d) have no progress mechanism and liveness fails.

- [ ] **Step 2: Add the leads-to liveness property**

Append to `ReversePairing.tla`:

```tla
(***************************************************************************)
(* Liveness: leads-to convergence.                                         *)
(*                                                                         *)
(* Any disagreement between desired and replicator eventually converges,   *)
(* given fairness on Deliver, Process, and ReceiveAck. Both install and    *)
(* teardown directions covered.                                            *)
(***************************************************************************)

InstallConverges ==
  \A n, p \in Node, c \in Collection :
    (c \in desired[n][p] /\ n # p) ~> (c \in replicator[p][n])

TeardownConverges ==
  \A n, p \in Node, c \in Collection :
    (c \notin desired[n][p] /\ c \in replicator[p][n]) ~> (c \notin replicator[p][n])

Convergence == InstallConverges /\ TeardownConverges
```

- [ ] **Step 3: Add the property to MC config**

Edit `MCReversePairing.cfg`:

```
PROPERTY Convergence
```

(Place this on its own line; do not repurpose the `INVARIANT` lines — invariants and properties are checked differently by TLC.)

- [ ] **Step 4: Run TLC with liveness**

```bash
cd crates/defra-agent/proofs/tla && ./scripts/run-tlc.sh MCReversePairing
```

Expected: longer runtime (target: < 15 minutes). Final output: `Model checking completed. No error has been found.` and `Temporal properties were violated.` will NOT appear.

- [ ] **Step 5: If liveness fails, diagnose**

Common failure modes:
- **Stuttering execution.** If TLC reports a counterexample where the system stutters indefinitely after a disagreement, fairness on the wrong action set is the cause. Verify the four `WF_vars` clauses cover Deliver, Process, ReceiveAck, AND Reconcile (the last being essential for post-`Crash` recovery).
- **Unbounded crashes.** If `MaxCrashes` is too high, the crash budget might allow many crashes in the abstract semantics (TLC respects the budget but liveness needs *eventual* quiescence between crashes). Lower to 1 if needed.
- **Drop without bound.** TLC's weak fairness on `Drop` is intentionally absent, but if `Drop` can fire infinitely on a specific message and `Deliver` is never enabled (e.g., `Drop` happens immediately after every `Send`), liveness fails. The fairness on `Deliver` should override this; if not, investigate enabling-condition mismatches.
- **RPCId pool exhausted under fairness.** If `Reconcile` fairness forces firing whenever enabled and the model exhausts `RPCId` partway through, the system gets stuck and liveness reports a counterexample. Fix: bump `RPCId` cardinality in `.cfg`.

In all cases: do NOT add fairness to `Drop`. Drops are unreliable network behavior; fairness on `Deliver` is what guarantees eventual delivery.

- [ ] **Step 6: Commit**

```bash
git add crates/defra-agent/proofs/tla/ReversePairing.tla \
        crates/defra-agent/proofs/tla/MCReversePairing.cfg
git commit -m "$(cat <<'EOF'
Add fairness and leads-to convergence liveness

Weak fairness on Deliver, Process, ReceiveAck, and Reconcile (the
last essential for post-Crash recovery). InstallConverges and
TeardownConverges express that any desired/replicator disagreement
eventually resolves. TLC verifies under bounded parameters.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 11: Documentation pass — README, expected output, parameter knobs

Wrap up: capture how to run the spec, what each parameter knob does, and what the expected TLC output looks like. The next engineer (human or agent) needs to be able to reproduce results from a fresh checkout.

**Files:**
- Modify: `crates/defra-agent/proofs/tla/README.md`

- [ ] **Step 1: Expand README with sections for output, parameters, and known limitations**

Replace contents of `crates/defra-agent/proofs/tla/README.md`:

```markdown
# TLA+ Specs

Distributed-system verification for cross-node properties. Sibling to `../Proofs/` (per-node Lean specs) under issue #155's cross-boundary verification strategy.

See `../README.md` for how this fits the broader formal-verification model.

## Specs

- `ReversePairing` — control-plane convergence of subscription/replicator reverse-pairing between two peers. Spec design: `../../../../docs/superpowers/specs/2026-05-08-reverse-pairing-tla-design.md`.
- `Sanity` — toolchain smoke test; not a real model.

## One-time setup

```bash
./scripts/install-tools.sh
```

Downloads `tla2tools.jar` into `.tools/` (gitignored). Requires Java 11+ on `PATH`. Override version via `TLA_VERSION=v1.8.0`.

## Running

```bash
./scripts/run-tlc.sh MCReversePairing
```

The script runs TLC with parallel workers and writes state-graph artifacts to `states/` (gitignored).

## Parameters

Set in `MCReversePairing.cfg`:

| Constant | Default | Effect of raising |
|---|---|---|
| `Node` | `{A, B}` | State space ∝ |Node|² × ... |
| `Collection` | `{c1, c2}` | State space ∝ 2^|Collection| |
| `RPCId` | 8 ids | Allows more concurrent in-flight RPCs |
| `MaxCrashes` | 2 | Each crash adds branchings; bounded for finite checking |

Larger parameters increase state space exponentially. Benchmark before raising.

## Expected output

### Safety only

```
Computing initial states...
Finished computing initial states: 1 distinct state generated.
...
Model checking completed. No error has been found.
  Estimates of the probability that TLC did not check all reachable states
  ...
N states generated, M distinct states found, 0 states left on queue.
The depth of the complete state graph search is K.
```

For default parameters, expect `M` in the low hundreds and a runtime under a minute.

### With liveness

Liveness check adds a "Checking temporal properties" phase. Expect total runtime under 15 minutes for default parameters. Final line: `Model checking completed. No error has been found.`

A failure looks like:

```
Error: Temporal properties were violated.
The behavior up to this violation:
  State 1 ... State 2 ... ... [counterexample trace]
```

Interpret the trace by reading the actions taken between states and identifying where convergence stalled.

## What is checked

- **TypeOK** — every variable holds a value of its declared type
- **RPCIdsTracked** — every in-flight RPC has its id registered in `rpcIdsUsed`
- **RPCWellFormed** — RPC src/tgt distinct, kind valid, of-field consistent with kind
- **InFlightJustified** — every desired/replicator disagreement has a reconciling RPC somewhere
- **Convergence** (liveness, leads-to) — every disagreement eventually resolves

## Known limitations and deferred work

- **Single-RPC-per-Reconcile-firing.** `Reconcile` emits at most one Install or Teardown RPC per firing. Multiple disagreements get reconciled across multiple firings under fairness. Real defra-agent reconciles a batch per cycle; the model approximates with multiple firings.
- **Supporting invariant qualified.** `InFlightJustified` allows a disjunctive case "Reconcile is enabled" so the invariant holds in the OperatorWrite/Reconcile window and after `Crash` + `Drop`. Phase-variable formulation (where the window is explicit state) deferred.
- **Provenance.** Not modeled here. The structural-safety invariants check that state changes are mediated only by the right actions, but a full provenance proof — every replicator entry traces back to a prior `desired`-then-`Process` chain — is deferred to a future TLAPS proof or trace-inspection harness.
- **Set semantics for `messages`** rather than multiset; assumes RPC ids are unique (which the model enforces). Real network duplicates can be modeled via `Send` re-emitting under different ids.
- **No N > 2 fanout, no data-plane replication, no authorization correctness.** All explicit non-goals from the spec.

## Refining the model

If TLC finds a real bug:

1. Inspect the counterexample trace — which action transitions led to violation?
2. Decide: is the model wrong (over-permissive transition relation) or is the property over-stated?
3. Fix the model and re-run; never silently weaken the property to make a violation go away.
4. Document the diagnosis in the commit message.
```

- [ ] **Step 2: Run TLC one final time to confirm everything still works**

```bash
cd crates/defra-agent/proofs/tla && ./scripts/run-tlc.sh MCReversePairing
```

Expected: clean finish, all invariants and properties pass.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/tla/README.md
git commit -m "$(cat <<'EOF'
Document ReversePairing TLA+ spec — README, parameters, limitations

Captures how to run, what parameters control, expected output shape,
and the known limitations of the first-pass model (single-RPC-per-
Reconcile, no provenance, set-semantics messages, two-peer scope).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Self-Review Notes

**Spec coverage check.** Each requirement from `2026-05-08-reverse-pairing-tla-design.md` is implemented:

| Spec requirement | Plan task |
|---|---|
| State variables (`desired`, `replicator`, `inFlight`, `pendingInbound`, `messages`) | Task 2 |
| RPC kinds (Install, Teardown, Ack) | Task 2 |
| `OperatorWrite` (atomic update of `desired` only) | Task 3 |
| `Reconcile` for both install and teardown directions, fires from any state | Task 3 |
| `Send` / `Deliver` / `Drop` | Tasks 3 (Send via Reconcile), 4 (Deliver, Drop) |
| `Process` with persist-before-ack | Task 5 |
| `ReceiveAck` (no persisted state change on caller) | Task 6 |
| `Timeout` (liveness-only, no state change beyond `inFlight`) | Task 6 |
| `Crash` (clears in-memory; preserves persisted; recovery via Reconcile) | Task 7 |
| Modeling assumptions: handler idempotency, persist-before-ack, eventually-healthy network, finite crashes | Tasks 5 (idempotency built into Process), 5 (persist-before-ack atomic), 10 (fairness on Deliver), 7 (MaxCrashes) |
| Safety: structural and provenance invariants | Task 8 (structural; provenance deferred per decision 5) |
| Liveness: leads-to convergence (with fairness on `Reconcile`) | Task 10 |
| Supporting invariant: in-flight justification (qualified disjunctively) | Task 9 |
| Boundary discipline on timeouts | Task 6 (Timeout has no state change beyond `inFlight`) |

**Placeholder scan.** No "TBD" or "implement later" in this plan. All TLA+ code is concrete; all commands have explicit expected output.

**Type consistency.** Variable names match across tasks (`desired`, `replicator`, `inFlight`, `pendingInbound`, `messages`, `crashCount`, `rpcIdsUsed`). Action signatures match. RPC field names (`id`, `kind`, `src`, `tgt`, `collection`, `of`) consistent across `OperatorWrite`, `Reconcile{Install,Teardown}`, `Process`, `ackOf`, and the invariant predicates.

**Known plan-level concerns to surface during execution:**

- **State-space size.** Splitting `OperatorWrite` and `Reconcile` into independent actions enlarges the reachable state space (the OperatorWrite/Reconcile window adds states). If TLC times out under default parameters, lower `MaxCrashes` to 1 first, then reduce `RPCId` cardinality, then drop a collection.
- **`CHOOSE i \in RPCId \ rpcIdsUsed : TRUE`** is deterministic per state. With a single Reconcile firing per step, this is fine. If a future refactor allows multiple RPCs per firing, replace with a non-deterministic id-assignment pattern.
- **Reconcile fairness via `\A n \in Node : WF_vars(Reconcile(n))`.** Per-node weak fairness means each node's reconcile eventually fires when enabled. If TLC reports a liveness counterexample where one node never reconciles despite being enabled, verify the per-node quantifier is outside `WF_vars` (correct: `\A n : WF_vars(Reconcile(n))`) rather than inside (incorrect: `WF_vars(\A n : Reconcile(n))` would mean a different thing).
