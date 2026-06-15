# Network-Membership PR Cut 1 — Lean Model + Conformance Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the self-asserted peer-registry derivation in the Lean discovery model with a **signed network-membership** derivation, including explicit request/approve transitions, proving the design spec §9 obligations, and fence the predicates against pure Rust decision functions.

**Architecture:** Evolve `Proofs/PeerRegistryDiscovery/` in place (keep the namespace, barrel, and `structure.rs` registration unchanged). The proven ownership/nonce machinery is reused; only the *derivation source* changes — from live registry rows (`deriveRegistryDesired`) to membership materialization (`validNetwork ∧ adminSignedMembership ∧ memberSignedEndpoint`). Signatures are modeled as **abstract booleans** on the records, never crypto, with an explicit `signedBy : Did` so "signed by *this network's* admin" is checkable. Cut 1 ships the model + a small pure Rust decision module the model fences (the full reconciler is cut 5).

**Tech Stack:** Lean 4 + Mathlib (`crates/defra-agent/proofs`), Rust decision module (`crates/defra-agent/src/agent/p2p_reconcile/`), conformance tests (`crates/defra-agent/tests/conformance/`).

**Proof-body note (read once):** Lean theorem *statements*, definitions, and non-vacuity *witnesses* are written literally here. Theorem *proof bodies* (tactic blocks) are developed interactively against the build gate — the fence is `lake build` with **zero `sorry`** plus a satisfiable witness for every implication. A theorem that builds only because its hypothesis is unsatisfiable is a failure; each obligation pairs with a `*_witness`. Mirrors how `replay_rejected`/`reciprocal_join_still_gated` were built.

**Build-gate note (Finding 6):** Intermediate tasks that knowingly leave downstream files broken use **module-level** builds (`lake build Proofs.PeerRegistryDiscovery.State`), which compile only the named module + its deps. The **full** `lake build` (whole library, zero-sorry) is reserved for the first coherent point — end of Task 5.

**Open decision (Finding 4 — endpoint scoping):** Plan as written keeps `PeerEndpoint` **global / `did`-unique** per the finalized spec §78–79 (reachability is one per-node fact; materialization is network-scoped via the *membership's* `networkId`, so a DID materializes only where admitted). If the spec is revised to network-scope endpoints, add `ep.networkId = s.network.networkId` to `memberSignedEndpoint`/`deriveMaterializable` and a `networkId` field to `Endpoint`/`EndpointDecision` — a one-conjunct change isolated to Tasks 1–2 and 6.

**Worktree:** `defra-agent-network-membership` (branch `network-membership`, off merged main `c9640301`).

---

## File Structure

- `proofs/Proofs/PeerRegistryDiscovery/State.lean` — network-membership entities (`Network`, `Membership`, `Endpoint`, `JoinRequest`) with abstract sig bits + `signedBy`; membership/endpoint/network/request fields on `DiscoveryState`; `wellFormed` uniqueness invariant; `deriveMaterializable`. Remove `RegistryEntry`/`deriveRegistryDesired` and their theorems.
- `proofs/Proofs/PeerRegistryDiscovery/Transition.lean` — the five predicates + `Decidable`; `submitRequest`/`approveRequest`/`revoke` transitions; rewrite `deriveStep` to `deriveMaterializable`; keep join/nonce/reciprocal transitions.
- `proofs/Proofs/PeerRegistryDiscovery/Derivation.lean` — obligations O1–O5 + witnesses; carry over `ownership_safe`, `replay_rejected`, reciprocal theorems (re-prove against the new derivation); `wellFormed` preservation lemmas.
- `proofs/Proofs/PeerRegistryDiscovery/Executable.lean` — decision mirrors + agreement lemmas.
- `crates/defra-agent/src/agent/p2p_reconcile/network_membership.rs` (CREATE) — pure decision fns over plain structs (incl. `signed_by`). No DB, no reconciler.
- `crates/defra-agent/tests/conformance/peer_registry_discovery.rs` — **fold** the membership decision-fn fences into this existing module (Finding 7: `Home::Module` allows one path per model; do NOT add a new module file or touch `structure.rs`/`conformance.rs`).

---

## Task 1: Network-membership state model + well-formedness (Lean)

**Files:** Modify `proofs/Proofs/PeerRegistryDiscovery/State.lean`.

- [ ] **Step 1: Add entity records.** After the `Nonce` abbrev:

```lean
abbrev NetworkId := String

/-- The network document. `adminSigValid` abstracts "admin_sig verifies for adminDid". -/
structure Network where
  networkId : NetworkId
  adminDid  : Did
  adminSigValid : Bool
  deriving DecidableEq, Repr

/-- Admin-authored membership grant. `signedBy` is the DID that signed it and
`adminSigValid` abstracts that signature's validity — so "signed by THIS network's
admin" is `adminSigValid ∧ signedBy = network.adminDid` (Finding 5). `active=false`
is a revoked tombstone. -/
structure Membership where
  networkId : NetworkId
  memberDid : Did
  active    : Bool
  signedBy  : Did
  adminSigValid : Bool
  deriving DecidableEq, Repr

/-- Member-self-asserted transport binding (global per node — Finding 4 / spec §78).
`memberSigValid` abstracts the member's DID signature; `fresh` folds liveness. -/
structure Endpoint where
  did   : Did
  peer  : PeerId
  fresh : Bool
  memberSigValid : Bool
  deriving DecidableEq, Repr

/-- Candidate-authored join request (Finding 3). `reqSigValid` abstracts the
candidate's signature. A request alone NEVER creates a membership; only an admin
`approveRequest` does. -/
structure JoinRequest where
  networkId    : NetworkId
  candidateDid : Did
  reqSigValid  : Bool
  deriving DecidableEq, Repr
```

- [ ] **Step 2: Replace `DiscoveryState`'s registry with the membership world:**

```lean
structure DiscoveryState where
  self : PeerId
  network : Network
  memberships : Finset Membership
  endpoints : Finset Endpoint
  requests : Finset JoinRequest
  operatorDesired : Finset PeerId
  registryDesired : Finset PeerId   -- now network-derived; name kept to limit churn
  consumedNonces : Finset Nonce
  deriving DecidableEq
```

- [ ] **Step 3: Add the well-formedness invariant (Finding 2 / Invariant 1):** at most one membership per `(networkId, memberDid)`.

```lean
/-- Schema-level invariant: NetworkMembership is unique on (networkId, memberDid). -/
def wellFormed (s : DiscoveryState) : Prop :=
  ∀ m₁ ∈ s.memberships, ∀ m₂ ∈ s.memberships,
    m₁.networkId = m₂.networkId → m₁.memberDid = m₂.memberDid → m₁ = m₂

instance (s : DiscoveryState) : Decidable s.wellFormed := by unfold wellFormed; infer_instance
```

- [ ] **Step 4: Add `deriveMaterializable`** (replaces `deriveRegistryDesired`). Membership carries the network scope; the endpoint stays global:

```lean
/-- Network-owned desired peers = peers of endpoints whose DID has an active,
admin-signed membership in this valid network, with a fresh member-signed binding,
peer ≠ self. The network scope lives on the membership (Finding 4: endpoint global). -/
def deriveMaterializable (s : DiscoveryState) : Finset PeerId :=
  (s.endpoints.filter (fun ep =>
      ep.fresh = true ∧ ep.memberSigValid = true ∧ ep.peer ≠ s.self ∧
      s.network.adminSigValid = true ∧
      s.memberships.toList.any (fun m =>
        m.memberDid = ep.did ∧ m.active = true ∧
        m.adminSigValid = true ∧ m.signedBy = s.network.adminDid ∧
        m.networkId = s.network.networkId))).image Endpoint.peer
```

- [ ] **Step 5: Re-home `settled`/`settle`/`effectiveDesired`** to `deriveMaterializable`; delete `RegistryEntry`, `Registry`, `deriveRegistryDesired`.
- [ ] **Step 6: Module-level build.** `cd crates/defra-agent/proofs && lake build Proofs.PeerRegistryDiscovery.State` — State compiles. (Downstream files still broken; full build deferred to Task 5 per the build-gate note.)
- [ ] **Step 7: Commit.** `proof(network): membership/endpoint/request state model + wellFormed + deriveMaterializable`

## Task 2: Predicates + request/approve/revoke transitions (Lean)

**Files:** Modify `proofs/Proofs/PeerRegistryDiscovery/Transition.lean`.

- [ ] **Step 1: The five predicates** (spec §9), each with `Decidable` via `unfold; infer_instance`:

```lean
def validNetwork (n : Network) : Prop := n.adminSigValid = true

def adminSignedMembership (m : Membership) (n : Network) : Prop :=
  m.adminSigValid = true ∧ m.signedBy = n.adminDid ∧ m.networkId = n.networkId   -- Finding 5

def memberSignedEndpoint (ep : Endpoint) : Prop :=
  ep.memberSigValid = true ∧ ep.fresh = true

def admittedMember (did : Did) (s : DiscoveryState) : Prop :=
  validNetwork s.network ∧
  ∃ m ∈ s.memberships, m.memberDid = did ∧ m.active = true ∧ adminSignedMembership m s.network

def materializableEndpoint (ep : Endpoint) (s : DiscoveryState) : Prop :=
  admittedMember ep.did s ∧ memberSignedEndpoint ep ∧ ep.peer ≠ s.self
```

- [ ] **Step 2: The bridge lemma** (used by O1/O2):

```lean
theorem mem_deriveMaterializable {s : DiscoveryState} {p : PeerId} :
    p ∈ deriveMaterializable s ↔
      ∃ ep ∈ s.endpoints, materializableEndpoint ep s ∧ ep.peer = p
```

- [ ] **Step 3: Mutators + transitions.** Define mutators as **key-based upserts over `(networkId, memberDid)`** (Finding 3 — matches the schema's unique index, so `wellFormed` is preserved by construction, not by a side hypothesis):

```lean
/-- Remove any membership for the same (networkId, memberDid), then insert `m`.
Key-based upsert ⇒ at most one row per key ⇒ wellFormed preserved. -/
def upsertMembership (s : DiscoveryState) (m : Membership) : DiscoveryState :=
  { s with memberships :=
      insert m (s.memberships.filter (fun x =>
        ¬ (x.networkId = m.networkId ∧ x.memberDid = m.memberDid))) }

def submitRequestState (s : DiscoveryState) (req : JoinRequest) : DiscoveryState :=
  { s with requests := insert req s.requests }            -- touches requests ONLY

def approveMembershipState (s : DiscoveryState) (m : Membership) : DiscoveryState :=
  upsertMembership s m

/-- Revocation is an admin-signed status=revoked UPSERT (spec §4): the tombstone
`tomb` carries active=false and the admin signature; it REPLACES the active grant. -/
def revokeState (s : DiscoveryState) (tomb : Membership) : DiscoveryState :=
  upsertMembership s tomb

/-- A join redeems a single-use token: it consumes the nonce and mutates NOTHING
ELSE (no membership growth — grants come only via approveRequest). Keeps
`replay_rejected` meaningful without reintroducing registry-growth (Finding 7). -/
def joinState (s : DiscoveryState) (tok : Token) : DiscoveryState :=
  { s with consumedNonces := insert tok.nonce s.consumedNonces }
```

Extend the `Transition` inductive (note the network-id matching on `approveRequest`, Finding 2, and the admin-signature carried by `revoke`, Finding 4):

```lean
  | submitRequest {pre post} (req : JoinRequest) :
      post = submitRequestState pre req → Transition pre post     -- changes requests only
  | approveRequest {pre post} (req : JoinRequest) (m : Membership) :
      req ∈ pre.requests →
      req.reqSigValid = true →                          -- valid candidate request
      req.networkId = pre.network.networkId →           -- request is FOR this network (Finding 2)
      adminSignedMembership m pre.network →             -- grant signed by THIS admin (⇒ m.networkId = network.networkId)
      m.memberDid = req.candidateDid → m.active = true →
      post = approveMembershipState pre m → Transition pre post
  | revoke {pre post} (tomb : Membership) :
      adminSignedMembership tomb pre.network →           -- revocation is admin-signed (Finding 4)
      tomb.active = false →                              -- status = revoked
      post = revokeState pre tomb → Transition pre post
```

Keep `join`/`reciprocalJoin` (now `joinState`-based: nonce-only) and `operatorWrite`. Point `deriveStep` at `deriveMaterializable`. The join admission gate's membership arm uses `admittedMember` (issuer is an admitted member) or TOFU bootstrap on empty `memberships`.

- [ ] **Step 4: Module-level build.** `lake build Proofs.PeerRegistryDiscovery.Transition`.
- [ ] **Step 5: Commit.** `proof(network): membership predicates + request/approve/revoke transitions`

## Task 3: Carry over ownership/nonce/reciprocal + wellFormed preservation (Lean)

**Files:** Modify `proofs/Proofs/PeerRegistryDiscovery/Derivation.lean`.

- [ ] **Step 1: Re-prove derivation lemmas** against `deriveMaterializable`: `self_not_mem_derive`, `deriveMaterializable_idempotent`, `derive_settles`, `derive_convergent`.
- [ ] **Step 2: Keep `ownership_safe` (= O4):** every `Transition` leaves `operatorDesired` untouched — re-run `cases` over the constructors (now incl. `submitRequest`/`approveRequest`/`revoke`, all of which leave `operatorDesired` by `rfl`).
- [ ] **Step 3: Keep nonce/reciprocal theorems:** `replay_rejected` + `replay_rejected_witness`, `reciprocal_join_still_gated` + witnesses, `reciprocal_replay_rejected`. Re-prove witnesses using an admitted state built from a `Membership` (not a `RegistryEntry`).
- [ ] **Step 4: `wellFormed` preservation:** prove every transition preserves `wellFormed`. Because `approveRequest`/`revoke` go through `upsertMembership` (key-based — removes any same-key row before insert), uniqueness holds **by construction**; the key lemma is `upsertMembership_wellFormed` (an upsert into a well-formed set is well-formed). `submitRequest`/`join`/`reciprocalJoin`/`operatorWrite` don't touch `memberships`, so they preserve it trivially.

```lean
theorem upsertMembership_wellFormed {s : DiscoveryState} (m : Membership)
    (hwf : s.wellFormed) : (upsertMembership s m).wellFormed
theorem transition_preserves_wellFormed {pre post : DiscoveryState}
    (hwf : pre.wellFormed) (h : Transition pre post) : post.wellFormed
```

- [ ] **Step 5: Module-level build** `lake build Proofs.PeerRegistryDiscovery.Derivation`; `grep -rn sorry Proofs/PeerRegistryDiscovery/Derivation.lean` clean.
- [ ] **Step 6: Commit.** `proof(network): ownership/nonce/reciprocal carried over; wellFormed preservation`

## Task 4: Obligations O1–O5 + witnesses (Lean)

**Files:** Modify `proofs/Proofs/PeerRegistryDiscovery/Derivation.lean`.

- [ ] **Step 1: O1 — soundness (Finding 1: nothing materializes without an admitted, signed endpoint).** This is the forward direction of `mem_deriveMaterializable`; state it standalone + a witness:

```lean
theorem materialized_implies_admitted {s : DiscoveryState} {p : PeerId}
    (h : p ∈ deriveMaterializable s) :
    ∃ ep ∈ s.endpoints, admittedMember ep.did s ∧ memberSignedEndpoint ep ∧ ep.peer = p
-- O1_witness: a state with an unsigned/unadmitted endpoint whose peer is NOT in
--   deriveMaterializable s (no other endpoint covers that peer) — realizable hypothesis.
```

- [ ] **Step 2: O2 — completeness (active admin-signed + fresh member-signed ⇒ materialized):**

```lean
theorem materializable_is_derived {s : DiscoveryState} {ep : Endpoint}
    (hep : ep ∈ s.endpoints) (h : materializableEndpoint ep s) :
    ep.peer ∈ deriveMaterializable s
-- O2_witness: concrete valid network + active membership (signedBy = adminDid) +
--   fresh signed endpoint ⇒ peer IS derived (genuine positive, not vacuous).
```

- [ ] **Step 3: O3 — revocation retracts only the peers *exclusively* materialized via the revoked DID (Finding 1 + 2), under `wellFormed`.** A peer materialized *both* via the revoked DID and via another admitted DID must NOT drop — so the filter keeps `p` iff a *surviving* (non-revoked-DID) endpoint still materializes it:

```lean
theorem revoke_retracts_exactly {pre post : DiscoveryState} {tomb : Membership}
    (hwf : pre.wellFormed) (hsig : adminSignedMembership tomb pre.network)
    (hrev : tomb.active = false) (h : post = revokeState pre tomb) :
    deriveMaterializable post =
      (deriveMaterializable pre).filter (fun p =>
        ∃ ep ∈ pre.endpoints,
          ep.did ≠ tomb.memberDid ∧ ep.peer = p ∧ materializableEndpoint ep pre)
-- Peers materialized ONLY via tomb.memberDid drop; peers also reachable via another
-- admitted DID stay (Finding 1: a co-peer admitted endpoint preserves p).
-- O3_witness: members A,B with DISTINCT peers, revoke A ⇒ A's peer drops, B's stays;
--   plus a shared-peer case: A,B both materialize peer p, revoke A ⇒ p stays (via B).
```

- [ ] **Step 4: O5 — a forged/unsigned join request cannot produce a grant (Finding 3):** the only membership-creating transition is `approveRequest`, which demands `adminSignedMembership m pre.network` (so `m.signedBy = adminDid ∧ m.adminSigValid`) and a `reqSigValid` request.

```lean
theorem membership_grant_requires_admin_signature {pre post : DiscoveryState}
    (h : Transition pre post) {m : Membership}
    (hnew : m ∈ post.memberships) (hold : m ∉ pre.memberships) :
    adminSignedMembership m pre.network
-- O5_witness_neg: a submitRequest with reqSigValid=false leaves memberships unchanged
--   (no grant); a submitRequest alone (no approve) leaves memberships unchanged.
```

- [ ] **Step 5: Module-level build.** `lake build Proofs.PeerRegistryDiscovery.Derivation`; `grep -rn sorry Proofs/PeerRegistryDiscovery/Derivation.lean` clean. (Executable.lean still references the old `TransitionKind`, so the *full* library build is deferred to Task 5 — the first point all four files are coherent.) Audit non-vacuity: O2/O3 witnesses are genuine positives; O1/O5 hypotheses are realizable.
- [ ] **Step 6: Commit.** `proof(network): obligations O1-O5 (soundness, completeness, revocation, admission) with witnesses`

## Task 5: Executable mirror + agreement (Lean)

**Files:** Modify `proofs/Proofs/PeerRegistryDiscovery/Executable.lean`.

- [ ] **Step 1: Decision fns + agreement** (mirror `decideAdmitsJoin`/`_agrees`):

```lean
def decideValidNetwork (n : Network) : Bool := n.adminSigValid
def decideAdmittedMember (did : Did) (s : DiscoveryState) : Bool := decide (admittedMember did s)
def decideMaterializable (ep : Endpoint) (s : DiscoveryState) : Bool := decide (materializableEndpoint ep s)
theorem decideAdmittedMember_agrees (did) (s) :
    decideAdmittedMember did s = true ↔ admittedMember did s := by
  unfold decideAdmittedMember; exact decide_eq_true_iff
theorem decideMaterializable_agrees (ep) (s) :
    decideMaterializable ep s = true ↔ materializableEndpoint ep s := by
  unfold decideMaterializable; exact decide_eq_true_iff
```

- [ ] **Step 2: Update `TransitionKind`** for the new constructors (`submitRequest`/`approveRequest`/`revoke`); keep `fromString_toString` by `cases k <;> rfl`.
- [ ] **Step 3: Full build** `lake build` zero-sorry.
- [ ] **Step 4: Commit.** `proof(network): executable membership decisions + agreement lemmas`

## Task 6: Rust decision module (pure)

**Files:** Create `crates/defra-agent/src/agent/p2p_reconcile/network_membership.rs`; add `mod network_membership;` to `mod.rs`.

- [ ] **Step 1: Failing test** (module `#[cfg(test)]`). The Lean `admittedMember` is **existential over memberships**, so the Rust API splits into a single-row predicate (`membership_admits_did`, ↔ Lean `adminSignedMembership ∧ active ∧ memberDid=did`) and the existential wrapper (`admitted_member`, ↔ Lean `admittedMember`) over a slice (Finding 6):

```rust
#[test]
fn admitted_is_existential_over_signed_active_memberships() {
    let net = NetworkDecision { admin_did: "did:a".into(), admin_sig_valid: true };
    let good = MembershipDecision {
        member_did: "did:x".into(), network_match: true, active: true,
        admin_sig_valid: true, signed_by: "did:a".into(),
    };
    // single-row predicate
    assert!(membership_admits_did(&net, &good, "did:x"));
    assert!(!membership_admits_did(&net, &MembershipDecision { signed_by: "did:evil".into(), ..good.clone() }, "did:x"));
    assert!(!membership_admits_did(&net, &MembershipDecision { active: false, ..good.clone() }, "did:x"));
    // existential wrapper: false on empty / no-match, true when some row admits
    assert!(!admitted_member(&net, &[], "did:x"));
    assert!(admitted_member(&net, &[good.clone()], "did:x"));
    assert!(!admitted_member(&net, &[good.clone()], "did:other"));
    // invalid network ⇒ never admitted regardless of memberships
    assert!(!admitted_member(&NetworkDecision { admin_sig_valid: false, ..net.clone() }, &[good], "did:x"));
}
```

- [ ] **Step 2: Run, confirm fail.** `cargo test -p defra-agent network_membership`.
- [ ] **Step 3: Implement** the pure fns over `Clone` input structs (`NetworkDecision`, `MembershipDecision`, `EndpointDecision`), boolean conjunctions matching the Lean predicates exactly. No DB/GraphQL; mirror `decide_join_admission`'s placement.
  - `valid_network(&NetworkDecision) -> bool` = `admin_sig_valid`.
  - `admin_signed_membership(&NetworkDecision, &MembershipDecision) -> bool` = `admin_sig_valid && signed_by == admin_did && network_match`.
  - `membership_admits_did(&NetworkDecision, &MembershipDecision, did) -> bool` = `admin_signed_membership(..) && m.active && m.member_did == did`.
  - `admitted_member(&NetworkDecision, &[MembershipDecision], did) -> bool` = `valid_network(net) && memberships.iter().any(|m| membership_admits_did(net, m, did))` (the existential wrapper ↔ Lean `admittedMember`).
  - `member_signed_endpoint(&EndpointDecision) -> bool` = `member_sig_valid && fresh`.
  - `materializable_endpoint(net, memberships, &EndpointDecision) -> bool` = `admitted_member(net, memberships, ep.did) && member_signed_endpoint(ep) && !ep.peer_is_self`.
- [ ] **Step 4: Run, confirm pass.** `cargo test -p defra-agent network_membership`.
- [ ] **Step 5: Commit.** `feat(p2p): pure network-membership decision functions (model-fenced)`

## Task 7: Conformance fence (folded) + final gate

**Files:** Modify `crates/defra-agent/tests/conformance/peer_registry_discovery.rs` (fold in — Finding 7; no new file, no `structure.rs`/`conformance.rs` change).

- [ ] **Step 1: Add conformance tests** in `peer_registry_discovery.rs` mirroring each predicate's truth table against the Rust decision fn: enumerate the deciding boolean/`signed_by` combinations for `admitted_member`/`materializable_endpoint` and assert Rust == the Lean predicate's expected value; include the revocation case (`active=false ⇒ not materializable`), the wrong-admin case (`signed_by != admin_did ⇒ not admitted`), and the forged case (`admin_sig_valid=false ⇒ not admitted`).
- [ ] **Step 2: Final gate.** `cd crates/defra-agent/proofs && lake build` (zero sorry; `grep -rn sorry Proofs/PeerRegistryDiscovery/` clean) + `cargo test -p defra-agent --test conformance peer_registry` + `cargo test -p defra-agent` (full package).
- [ ] **Step 3: Commit.** `test(network): fold membership decision fences into peer_registry_discovery conformance`

---

## Self-Review (spec §9 coverage + review findings)

| Item | Task | Note |
|---|---|---|
| `validNetwork`/`adminSignedMembership`/`memberSignedEndpoint`/`admittedMember`/`materializableEndpoint` | T2 S1 | `adminSignedMembership` requires `signedBy = adminDid` (Finding 5) |
| O1 forged/unsigned never materialized | T4 S1 | restated as soundness `materialized_implies_admitted` (Finding 1) |
| O2 active+fresh ⇒ materialized | T4 S2 | non-vacuous positive |
| O3 revocation retracts exactly | T4 S3 | by (networkId, memberDid) under `wellFormed` (Finding 2) |
| O4 ownership-safe | T3 S2 | `ownership_safe` |
| O5 forged request ⇏ grant | T4 S4 | real `submitRequest`/`approveRequest` transitions (Finding 3) |
| Executable mirror + agreement | T5 | |
| Conformance fence | T6 + T7 | folded into one module (Finding 7) |
| Build gates | all | module-level intermediate, full at T4/T5 (Finding 6) |
| Endpoint scoping | T1/T2 | global per spec §78; flagged open decision (Finding 4) |

Carried over (re-proven, not new obligations): `replay_rejected`, reciprocal gating, `wellFormed` preservation (T3). Deferred to later cuts: the SDL collections + protocol signing helpers (cut 2), CLI (cuts 3–4), reconciler wiring `deriveMaterializable` over real documents (cut 5), revoke/list/rm + e2e (cut 6).

**Placeholder scan:** proof bodies per the proof-body note; all definitions, statements, witnesses, Rust signatures, gates concrete. **Type consistency:** Rust `NetworkDecision{admin_did,admin_sig_valid}` / `MembershipDecision{member_did,network_match,active,admin_sig_valid,signed_by}` / `EndpointDecision{did,peer,member_sig_valid,fresh,peer_is_self}` ↔ Lean `Network`/`Membership`/`Endpoint` (`signed_by`↔`signedBy`, `admin_sig_valid`↔`adminSigValid`, `member_sig_valid`↔`memberSigValid`, `member_did`↔`memberDid`). The Lean existential `admittedMember` ↔ Rust `admitted_member(net, &[Membership], did)`; the single-row `adminSignedMembership` ↔ Rust `membership_admits_did`. Transition mutators (`upsertMembership`/`submitRequestState`/`approveMembershipState`/`revokeState`/`joinState`) are referenced consistently across Tasks 2–4.
