# Skills Privilege Algebra (Lean Foundation) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Establish the formal source-of-truth in Lean for defra-agent skills — prove that skill activation can never widen a behavior's tool surface beyond its `tool_selection` ceiling (the D3/S-Skill-1 privilege-monotonicity invariant), and that the candidate set respects the owning principal (D5/S-Skill-3).

**Architecture:** A single self-contained Lean module `Proofs/Skills.lean`, modeled in the same `Finset`-based style as `Proofs/Identity/State.lean` + `Proofs/Identity/Permission.lean`. It defines a behavior's resolved tool *ceiling*, a `Skill` with declared tool *refs* (D3: dependencies, never grants), the D5 effective candidate-set filter (`scope: principal|behavior` + `skill_refs`/`skill_excludes`), and the activation function (`ceiling ∪ ⋃ (toolRefs ∩ ceiling)`), then proves the activation result is `⊆ ceiling`. No Rust changes, no apply-path changes — this is the foundation the later slices consume.

**Tech Stack:** Lean 4 (`leanprover/lean4:v4.18.0`), Lake, mathlib (`Finset`). Build via `lake build`.

**Sequence context:** This is **plan 1 of 5** for the skills spec (`docs/superpowers/specs/2026-06-02-skills-integration-design.md`). CLAUDE.md mandates anything that changes what invariants hold starts in Lean, and the apply path is strictly Lean-fenced (`crates/defra-agent-cli/src/config_import.rs:884`), so the privilege algebra lands first. Subsequent plans: (2) extend `ApplyReconcile` Lean + `Collection` enum + `desired_state` + apply wiring + Rust conformance binding; (3) prompt + tool-surface composition consuming this invariant; (4) Codex shim wiring; (5) `config skill import`/export.

---

### Task 0: Lean build environment in this worktree

The worktree has no mathlib build cache, and `lake exe cache get` crashes on macOS (dyld `SG_READ_ONLY`). The fix is to symlink a sibling worktree's already-built mathlib, then `lake build` reuses it.

**As-built note:** the worktree had no `.lake` directory at all, so rather than the build-dir-only symlink below, the actual realization symlinked the entire `packages` tree from the parent worktree (after confirming `lake-manifest.json` and `lean-toolchain` match byte-for-byte): `mkdir -p crates/defra-agent/proofs/.lake && ln -s /Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent/crates/defra-agent/proofs/.lake/packages crates/defra-agent/proofs/.lake/packages`. This pulls in mathlib + all transitive deps (source and build) in one step. The sanity build (Step 3) then confirms it.

**Files:**
- Create (symlink): `crates/defra-agent/proofs/.lake/packages/mathlib/.lake/build`

- [ ] **Step 1: Fetch the mathlib package source (no build) into this worktree**

Run from repo root:
```bash
cd crates/defra-agent/proofs && lake update -R 2>/dev/null; lake exe cache get 2>/dev/null; true
```
Expected: may print errors / crash on the cache step — that is fine, we only need the package directory `.lake/packages/mathlib` to exist. Verify:
```bash
ls -d crates/defra-agent/proofs/.lake/packages/mathlib
```
Expected: the directory path prints (exists).

- [ ] **Step 2: Symlink the parent worktree's built mathlib `build` dir**

```bash
SRC=/Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent/crates/defra-agent/proofs/.lake/packages/mathlib/.lake/build
DST=crates/defra-agent/proofs/.lake/packages/mathlib/.lake/build
rm -rf "$DST" && ln -s "$SRC" "$DST" && ls -ld "$DST"
```
Expected: prints a symlink `... build -> /Users/.../defra-agent/.../mathlib/.lake/build`.

- [ ] **Step 3: Verify the existing proofs build (sanity check the cache works)**

```bash
cd crates/defra-agent/proofs && lake build Proofs.Identity.Permission
```
Expected: builds with no errors (may print a few mathlib up-to-date lines). This confirms the cache symlink is usable before we add new code.

---

### Task 1: Define the Skills model (types + ceiling + skill)

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/Skills.lean`

- [ ] **Step 1: Write the model with the theorems stubbed as `sorry`**

Create `crates/defra-agent/proofs/Proofs/Skills.lean` with exactly:

```lean
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Union

/-!
# Skills — privilege algebra

Formal source-of-truth for defra-agent skills (spec
`docs/superpowers/specs/2026-06-02-skills-integration-design.md`).

A `Skill` declares the tools it *depends on* (`toolRefs`) — it never *grants*
them (D3, Codex-faithful). A behavior resolves a tool `ceiling` from its
`tool_selection`. Activation contributes, per skill, only `toolRefs ∩ ceiling`
(intersect + degrade), so the resolved surface with any skills active stays
`⊆ ceiling`. This module proves that privilege-monotonicity (S-Skill-1), the
composition closure under unions of active skills (S-Skill-2), and that the
D5 effective candidate set respects the owning principal (S-Skill-3).
-/

namespace Skills

abbrev ToolId := String
abbrev SkillId := String
abbrev Did := String

inductive Scope
  | principal
  | behavior
  deriving DecidableEq, Repr

/-- A skill: owned by a principal, scoped (D5), declaring tool dependencies. -/
structure Skill where
  id       : SkillId
  owner    : Did
  scope    : Scope
  toolRefs : Finset ToolId
  enabled  : Bool
  deriving DecidableEq

/-- A behavior: its resolved tool ceiling (D3) plus the D5 refinement lists. -/
structure Behavior where
  id            : String
  principal     : Did
  ceiling       : Finset ToolId
  skillRefs     : Finset SkillId
  skillExcludes : Finset SkillId

/-- D5 effective candidate set: principal-scoped skills inherit to every
    behavior of the owner; behavior-scoped skills are candidates only where
    opted in via `skillRefs`; `skillExcludes` opts out. -/
def candidates (skills : Finset Skill) (b : Behavior) : Finset Skill :=
  skills.filter (fun s =>
    s.owner = b.principal ∧
    s.enabled = true ∧
    (s.scope = Scope.principal ∨ s.id ∈ b.skillRefs) ∧
    s.id ∉ b.skillExcludes)

/-- Tools an active skill may use against a behavior ceiling: intersect +
    degrade (D3). Never adds a tool the behavior does not already allow. -/
def skillTools (b : Behavior) (s : Skill) : Finset ToolId :=
  s.toolRefs ∩ b.ceiling

/-- The tool surface available for a request with a set of `active` skills:
    the behavior ceiling plus each active skill's degraded contribution. -/
def resolvedSurface (b : Behavior) (active : Finset Skill) : Finset ToolId :=
  b.ceiling ∪ active.biUnion (skillTools b)

theorem activation_subset_ceiling (b : Behavior) (active : Finset Skill) :
    resolvedSurface b active ⊆ b.ceiling := by
  sorry

theorem candidates_respect_principal (skills : Finset Skill) (b : Behavior)
    {s : Skill} (hs : s ∈ candidates skills b) :
    s.owner = b.principal ∧ s.enabled = true := by
  sorry

end Skills
```

- [ ] **Step 2: Build to confirm the model elaborates (theorems still `sorry`)**

```bash
cd crates/defra-agent/proofs && lake build Proofs.Skills
```
Expected: builds successfully, printing `declaration uses 'sorry'` warnings for `activation_subset_ceiling` and `candidates_respect_principal`. No *errors* — this confirms the defs type-check and the lemma statements are well-formed before we prove them. (As-built note: `Finset.biUnion` + `Finset.biUnion_subset` live in `Mathlib.Data.Finset.Union`, which is why the second import is `import Mathlib.Data.Finset.Union` rather than a `Lattice.*` module.)

---

### Task 2: Prove S-Skill-1 (privilege monotonicity)

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Skills.lean` (replace the first `sorry`)

- [ ] **Step 1: Replace the `activation_subset_ceiling` proof**

Replace the `theorem activation_subset_ceiling ... := by sorry` block with:

```lean
theorem activation_subset_ceiling (b : Behavior) (active : Finset Skill) :
    resolvedSurface b active ⊆ b.ceiling := by
  unfold resolvedSurface
  apply Finset.union_subset (Finset.Subset.refl _)
  rw [Finset.biUnion_subset]
  intro s _
  unfold skillTools
  exact Finset.inter_subset_right
```

- [ ] **Step 2: Build to confirm the proof closes**

```bash
cd crates/defra-agent/proofs && lake build Proofs.Skills
```
Expected: builds with no error and **no** `sorry` warning for `activation_subset_ceiling` (the only remaining `sorry` warning is `candidates_respect_principal`).

If `Finset.inter_subset_right` errors about expected arguments, use the explicit form `exact Finset.inter_subset_right (s := s.toolRefs) (t := b.ceiling)`. If `Finset.biUnion_subset` rewrites the goal into an unexpected shape, replace the `rw [...]; intro s _` lines with `exact Finset.biUnion_subset.mpr (fun s _ => Finset.inter_subset_right)`.

---

### Task 3: Prove S-Skill-3 (candidate set respects principal) and add S-Skill-2

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Skills.lean` (replace the second `sorry`, add one theorem)

- [ ] **Step 1: Replace the `candidates_respect_principal` proof**

Replace the `theorem candidates_respect_principal ... := by sorry` block with:

```lean
theorem candidates_respect_principal (skills : Finset Skill) (b : Behavior)
    {s : Skill} (hs : s ∈ candidates skills b) :
    s.owner = b.principal ∧ s.enabled = true := by
  unfold candidates at hs
  rw [Finset.mem_filter] at hs
  exact ⟨hs.2.1, hs.2.2.1⟩
```

- [ ] **Step 2: Add S-Skill-2 (composition closure) below it, before `end Skills`**

```lean
/-- S-Skill-2: activating any subset of the candidate set still stays within
    the ceiling — no union of skills escalates privilege. -/
theorem composition_closed (skills : Finset Skill) (b : Behavior)
    (active : Finset Skill) (_hsub : active ⊆ candidates skills b) :
    resolvedSurface b active ⊆ b.ceiling :=
  activation_subset_ceiling b active
```

- [ ] **Step 3: Build to confirm all proofs close with zero `sorry`**

```bash
cd crates/defra-agent/proofs && lake build Proofs.Skills 2>&1 | tee /tmp/skills_build.log; grep -c "sorry" /tmp/skills_build.log || true
```
Expected: build succeeds; `grep -c "sorry"` prints `0`. If the `candidates_respect_principal` extraction errors (predicate conjunction nests differently than `hs.2.1`/`hs.2.2.1`), inspect with `obtain ⟨_hmem, howner, henabled, _, _⟩ := (Finset.mem_filter.mp hs)` and return `⟨howner, henabled⟩`.

---

### Task 4: Wire the module into the proofs aggregator and build everything

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs.lean` (add one import line)

- [ ] **Step 1: Add the import**

In `crates/defra-agent/proofs/Proofs.lean`, add this line in the import block (alphabetical neighborhood, e.g. after `import Proofs.Session.Properties` or near the other top-level modules — exact position is cosmetic):

```lean
import Proofs.Skills
```

- [ ] **Step 2: Build the full proof project**

```bash
cd crates/defra-agent/proofs && lake build
```
Expected: the entire `Proofs` target builds with no errors. (First full build may take longer as it elaborates all sibling modules against the cached mathlib; subsequent builds are incremental.)

- [ ] **Step 3: Confirm no new `sorry` was introduced project-wide by our module**

```bash
cd crates/defra-agent/proofs && grep -n "sorry" Proofs/Skills.lean || echo "NO SORRY IN Skills.lean"
```
Expected: prints `NO SORRY IN Skills.lean`.

---

### Task 5: Commit

- [ ] **Step 1: Commit the new module and aggregator wiring**

```bash
cd /Users/johnzampolin/go/src/github.com/sourcenetwork/defra-agent-skills-investigation
git add crates/defra-agent/proofs/Proofs/Skills.lean crates/defra-agent/proofs/Proofs.lean docs/superpowers/plans/2026-06-02-skills-privilege-algebra-lean.md
git commit -F - <<'EOF'
Prove skills privilege algebra in Lean (#340)

Proofs/Skills.lean models the skill privilege algebra that is the formal
source-of-truth for the skills spec (slice 3a):

- activation_subset_ceiling (S-Skill-1): skill activation never widens a
  behavior's tool surface beyond its tool_selection ceiling -- skills declare
  dependencies (toolRefs intersect ceiling, degrade), never grants (D3).
- composition_closed (S-Skill-2): no union of active skills escalates.
- candidates_respect_principal (S-Skill-3): the D5 effective candidate set
  (scope principal|behavior + skill_refs/skill_excludes) only ever ranges over
  the owning principal's enabled skills.

Self-contained Finset model mirroring Proofs/Identity. Zero sorry. Wired into
the Proofs aggregator; full `lake build` is green. Consumed by later slices
(Collection/apply plumbing, prompt+tool-surface composition).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
git log --oneline -1
```
Expected: commit succeeds; the new commit hash + subject print.

---

## Self-Review

**Spec coverage:** This plan implements the D4 commitment ("formalize the privilege algebra in Lean; keep the lifecycle declarative") — specifically S-Skill-1 (`activation_subset_ceiling`), S-Skill-2 (`composition_closed`), and S-Skill-3 (`candidates_respect_principal`). The D5 effective-set formula is encoded in `candidates`. The D3 intersect-and-degrade rule is encoded in `skillTools`. The `ApplyReconcile` ordering extension and the Rust conformance binding (also part of slice 3) are deferred to plan 2 (they require the `Collection` enum + `desired_state` changes), and are noted in the sequence context. No other spec section is in scope for this plan by design.

**Placeholder scan:** No TBD/TODO/"handle edge cases" steps. Every code step shows the complete Lean to write. Every command step shows the exact command and expected output. Lemma-name fallbacks are given inline for the two proofs most likely to need a mathlib-name adjustment.

**Type consistency:** `Skill`/`Behavior`/`Scope` field names (`owner`, `principal`, `ceiling`, `toolRefs`, `skillRefs`, `skillExcludes`, `scope`, `enabled`) are used identically across `candidates`, `skillTools`, `resolvedSurface`, and all four theorems. `resolvedSurface` takes `(b, active)` consistently in S-Skill-1 and S-Skill-2. `candidates` takes `(skills, b)` consistently in S-Skill-2 and S-Skill-3.
