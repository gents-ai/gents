import Proofs.Basic

/-! Providers share one HTTP transport. It persists the capture after body
rewrites and immediately before the network call; this is the provider-boundary
ordering modeled below. -/

namespace RenderedCapture

/-- The permission and audit boundary the capture is attributed to. -/
abbrev NodeDid := Nat

/-- Identity of exactly one provider attempt: node, session, request `_docID`
(DefraDB document identity, not the non-unique `AgentRequest.request_id` field),
turn, and attempt within the turn. The durable column encoding must be injective
on this tuple (`boundary.rendered-capture.key-encoding-injectivity`); the shipped
unescaped `"{session_id}:{tool_call_id}"` key is a live instance of that defect
class. -/
structure CaptureKey where
  nodeDid : NodeDid
  sessionId : SessionId
  requestId : RequestId
  turnIndex : Nat
  attempt : Nat
  deriving DecidableEq, Repr

structure CanonicalRequest where
  value : Nat
  deriving DecidableEq, Repr

/-! Commit witness equality is supplied by DefraDB; CID computation and
collision resistance are external database guarantees. Production's splice
encoder must also enforce bounds, Unicode boundaries, field removal, and byte
round trips; `splice_roundtrip` alone does not establish those properties. -/

abbrev FieldCommitWitness := Nat
abbrev ArtifactRef := Nat
abbrev CanonicalBytes := List Nat

def canonicalBytes (request : CanonicalRequest) : CanonicalBytes := [request.value]

def decodeCanonical : CanonicalBytes → Option CanonicalRequest
  | [value] => some { value }
  | _ => none

@[simp] theorem canonical_bytes_roundtrip (request : CanonicalRequest) :
    decodeCanonical (canonicalBytes request) = some request := by
  cases request <;> rfl

def splice (base : CanonicalBytes) (prefixLen suffixLen : Nat)
    (middle : CanonicalBytes) : Option CanonicalBytes :=
  if prefixLen + suffixLen ≤ base.length then
    some (base.take prefixLen ++ middle ++ base.drop (base.length - suffixLen))
  else none

theorem splice_roundtrip (base target middle : CanonicalBytes) (prefixLen suffixLen : Nat)
    (hBounds : prefixLen + suffixLen ≤ base.length)
    (hTarget : target = base.take prefixLen ++ middle ++ base.drop (base.length - suffixLen)) :
    splice base prefixLen suffixLen middle = some target := by
  simp [splice, hBounds, hTarget]

inductive StoredRequest where
  | legacyFull (bytes : CanonicalBytes)
  | full (bytes : CanonicalBytes)
  | delta (baseRef : ArtifactRef) (baseWitness : FieldCommitWitness)
      (prefixLen suffixLen : Nat) (middle : CanonicalBytes)
  /-- Integrity uses the database commit identity, never a self-attested
  digest. -/
  | manifest (blocks : List (ArtifactRef × FieldCommitWitness))
  deriving DecidableEq, Repr

structure StoredVersion where
  commit : FieldCommitWitness
  encoded : StoredRequest

abbrev ArtifactStore := ArtifactRef → Option StoredVersion

mutual

/-- Bounded recursive decode: the immutable base field commit must equal the
witness pinned by the delta; fuel bounds work and makes cycles fail closed. -/
def resolveBytes (store : ArtifactStore) : Nat → ArtifactRef →
    FieldCommitWitness → Option CanonicalBytes
  | 0, _, _ => none
  | fuel + 1, ref, expected => do
      let version ← store ref
      if version.commit != expected then none else
        match version.encoded with
        | .legacyFull bytes | .full bytes => some bytes
        | .delta baseRef baseWitness prefixLen suffixLen middle => do
            let base ← resolveBytes store fuel baseRef baseWitness
            splice base prefixLen suffixLen middle
        | .manifest blocks => do
            let chunks ← resolveBlocks store fuel blocks
            some (chunks.foldr (· ++ ·) [])

/-- Resolve the bytes of every block a manifest names, in list order. Fuel is
one shared budget across the manifest and everything it references; fuel `0`
fails closed, including for the empty manifest. -/
def resolveBlocks (store : ArtifactStore) : Nat →
    List (ArtifactRef × FieldCommitWitness) → Option (List CanonicalBytes)
  | 0, _ => none
  | _, [] => some []
  | fuel + 1, (ref, witness) :: rest => do
      let chunk ← resolveBytes store fuel ref witness
      let chunks ← resolveBlocks store fuel rest
      some (chunk :: chunks)

end

def resolveRequest (store : ArtifactStore) (fuel : Nat) (ref : ArtifactRef)
    (expected : FieldCommitWitness) : Option CanonicalRequest := do
  let bytes ← resolveBytes store fuel ref expected
  decodeCanonical bytes

theorem full_resolves_exactly (store : ArtifactStore) (ref witness : Nat)
    (request : CanonicalRequest)
    (h : store ref = some { commit := witness, encoded := .full (canonicalBytes request) }) :
    resolveRequest store 1 ref witness = some request := by
  simp [resolveRequest, resolveBytes, h]

/-- This witness covers two manifest levels and one leaf; it is not a general
height theorem for the native reader’s eight-level bound. -/
theorem nested_manifest_resolves_exactly (store : ArtifactStore)
    (root rootWitness branch branchWitness leaf leafWitness : Nat)
    (request : CanonicalRequest)
    (hRoot : store root = some ⟨rootWitness, .manifest [(branch, branchWitness)]⟩)
    (hBranch : store branch = some ⟨branchWitness, .manifest [(leaf, leafWitness)]⟩)
    (hLeaf : store leaf = some ⟨leafWitness, .full (canonicalBytes request)⟩) :
    resolveRequest store 5 root rootWitness = some request := by
  simp [resolveRequest, resolveBytes, resolveBlocks, hRoot, hBranch, hLeaf,
    canonical_bytes_roundtrip]

theorem witness_mismatch_fails_closed (store : ArtifactStore) (ref expected actual : Nat)
    (encoded : StoredRequest) (hNe : actual ≠ expected)
    (h : store ref = some { commit := actual, encoded }) :
    resolveRequest store 1 ref expected = none := by
  simp [resolveRequest, resolveBytes, h, hNe]

theorem no_fuel_fails_closed (store : ArtifactStore) (ref witness : Nat) :
    resolveRequest store 0 ref witness = none := by
  rfl

/-- `blocks` is stored, in list order aligned with `chunks`, as full documents
whose commits are exactly the pinned witnesses. -/
inductive FullBlocks (store : ArtifactStore) :
    List (ArtifactRef × FieldCommitWitness) → List CanonicalBytes → Prop where
  | nil : FullBlocks store [] []
  | cons (blockRef : ArtifactRef) (blockWitness : FieldCommitWitness)
      (chunk : CanonicalBytes) (rest : List (ArtifactRef × FieldCommitWitness))
      (chunks : List CanonicalBytes)
      (h : store blockRef = some { commit := blockWitness, encoded := .full chunk })
      (hTail : FullBlocks store rest chunks) :
      FullBlocks store ((blockRef, blockWitness) :: rest) (chunk :: chunks)

theorem FullBlocks.lengths {store : ArtifactStore}
    {blocks : List (ArtifactRef × FieldCommitWitness)} {chunks : List CanonicalBytes}
    (h : FullBlocks store blocks chunks) : blocks.length = chunks.length := by
  induction h with
  | nil => rfl
  | cons _ _ _ _ _ _ _ ih => simpa using ih

theorem resolveBlocks_of_full (store : ArtifactStore) :
    ∀ (blocks : List (ArtifactRef × FieldCommitWitness)) (chunks : List CanonicalBytes)
      (fuel : Nat), chunks.length + 1 ≤ fuel → FullBlocks store blocks chunks →
      resolveBlocks store fuel blocks = some chunks := by
  intro blocks
  induction blocks with
  | nil =>
      intro chunks fuel hFuel hFull
      cases hFull
      cases fuel with
      | zero => simp at hFuel
      | succ m => simp [resolveBlocks]
  | cons entry rest ih =>
      obtain ⟨headRef, headWitness⟩ := entry
      intro chunks fuel hFuel hFull
      cases hFull with
      | cons _ _ chunk _ chunks hHeadStore hTail =>
          cases fuel with
          | zero => simp at hFuel
          | succ m =>
              have hLe : chunks.length + 1 ≤ m := by
                simp only [List.length_cons] at hFuel; omega
              have hHead : resolveBytes store m headRef headWitness = some chunk := by
                cases m with
                | zero => omega
                | succ m' => simp [resolveBytes, hHeadStore]
              have hRest := ih chunks m hLe hTail
              simp only [resolveBlocks]
              simp [hHead, hRest]

theorem resolveBlocks_insufficient_fuel (store : ArtifactStore) :
    ∀ (blocks : List (ArtifactRef × FieldCommitWitness)) (fuel : Nat),
      fuel ≤ blocks.length → resolveBlocks store fuel blocks = none := by
  intro blocks
  induction blocks with
  | nil =>
      intro fuel hLe
      cases fuel with
      | zero => rfl
      | succ m => simp at hLe
  | cons entry rest ih =>
      obtain ⟨headRef, headWitness⟩ := entry
      intro fuel hLe
      cases fuel with
      | zero => rfl
      | succ m =>
          have hRestNone : resolveBlocks store m rest = none :=
            ih m (by simp only [List.length_cons] at hLe; omega)
          simp only [resolveBlocks]
          cases h : resolveBytes store m headRef headWitness <;> simp [hRestNone]

theorem resolveBytes_absent (store : ArtifactStore) :
    ∀ (fuel : Nat) (ref : ArtifactRef) (witness : FieldCommitWitness),
      (∀ encoded, store ref ≠ some { commit := witness, encoded }) →
      resolveBytes store fuel ref witness = none := by
  intro fuel
  induction fuel with
  | zero => intro _ _ _; rfl
  | succ m =>
      intro ref witness hAbsent
      cases hStore : store ref with
      | none => simp [resolveBytes, hStore]
      | some version =>
          have hNe : version.commit ≠ witness := by
            intro hEq
            exact hAbsent version.encoded (by
              rw [hStore]
              rw [← hEq])
          simp [resolveBytes, hStore, hNe]

theorem resolveBlocks_unresolvable (store : ArtifactStore) :
    ∀ (blocks : List (ArtifactRef × FieldCommitWitness)) (fuel : Nat)
      (blockRef : ArtifactRef) (blockWitness : FieldCommitWitness),
      (blockRef, blockWitness) ∈ blocks →
      (∀ encoded, store blockRef ≠ some { commit := blockWitness, encoded }) →
      resolveBlocks store fuel blocks = none := by
  intro blocks
  induction blocks with
  | nil => intro fuel blockRef blockWitness hMem _; cases hMem
  | cons entry rest ih =>
      obtain ⟨headRef, headWitness⟩ := entry
      intro fuel blockRef blockWitness hMem hAbsent
      cases fuel with
      | zero => rfl
      | succ m =>
          simp only [List.mem_cons] at hMem
          cases hMem with
          | inl hHead =>
              have hRefEq : blockRef = headRef := congrArg Prod.fst hHead
              have hWitEq : blockWitness = headWitness := congrArg Prod.snd hHead
              subst hRefEq
              subst hWitEq
              simp only [resolveBlocks]
              rw [resolveBytes_absent _ _ _ _ hAbsent]
              simp
          | inr hMemTail =>
              have hRestNone : resolveBlocks store m rest = none :=
                ih m blockRef blockWitness hMemTail hAbsent
              simp only [resolveBlocks]
              cases h : resolveBytes store m headRef headWitness <;> simp [hRestNone]

theorem manifest_of_fulls_resolves_exactly (store : ArtifactStore) (ref witness : Nat)
    (blocks : List (ArtifactRef × FieldCommitWitness)) (chunks : List CanonicalBytes)
    (request : CanonicalRequest)
    (hStore : store ref = some { commit := witness, encoded := .manifest blocks })
    (hBlocks : FullBlocks store blocks chunks)
    (hConcat : chunks.foldr (· ++ ·) [] = canonicalBytes request) :
    resolveRequest store (blocks.length + 2) ref witness = some request := by
  have hLen := hBlocks.lengths
  have hBlocks' : resolveBlocks store (blocks.length + 1) blocks = some chunks :=
    resolveBlocks_of_full store blocks chunks _ (by omega) hBlocks
  show resolveRequest store (blocks.length + 1 + 1) ref witness = some request
  simp [resolveRequest, resolveBytes, hStore, hBlocks', hConcat]

theorem manifest_witness_mismatch_fails_closed (store : ArtifactStore)
    (fuel ref witness : Nat) (blocks : List (ArtifactRef × FieldCommitWitness))
    (blockRef blockWitness : Nat) (version : StoredVersion)
    (hStored : store ref = some { commit := witness, encoded := .manifest blocks })
    (hListed : (blockRef, blockWitness) ∈ blocks)
    (hMismatch : version.commit ≠ blockWitness)
    (hBlock : store blockRef = some version) :
    resolveRequest store fuel ref witness = none := by
  have hAbsent : ∀ encoded, store blockRef ≠ some { commit := blockWitness, encoded } := by
    intro encoded hEq
    rw [hBlock] at hEq
    injection hEq with hVer
    exact hMismatch (by rw [hVer])
  cases fuel with
  | zero => rfl
  | succ m =>
      have hNone : resolveBlocks store m blocks = none :=
        resolveBlocks_unresolvable store blocks m blockRef blockWitness hListed hAbsent
      simp [resolveRequest, resolveBytes, hStored, hNone]

theorem manifest_missing_ref_fails_closed (store : ArtifactStore)
    (fuel ref witness : Nat) (blocks : List (ArtifactRef × FieldCommitWitness))
    (blockRef blockWitness : Nat)
    (hStored : store ref = some { commit := witness, encoded := .manifest blocks })
    (hListed : (blockRef, blockWitness) ∈ blocks)
    (hMissing : store blockRef = none) :
    resolveRequest store fuel ref witness = none := by
  have hAbsent : ∀ encoded, store blockRef ≠ some { commit := blockWitness, encoded } := by
    intro encoded hEq
    rw [hMissing] at hEq
    exact Option.noConfusion hEq
  cases fuel with
  | zero => rfl
  | succ m =>
      have hNone : resolveBlocks store m blocks = none :=
        resolveBlocks_unresolvable store blocks m blockRef blockWitness hListed hAbsent
      simp [resolveRequest, resolveBytes, hStored, hNone]

theorem manifest_no_fuel_fails_closed (store : ArtifactStore) (ref witness : Nat)
    (blocks : List (ArtifactRef × FieldCommitWitness))
    (hStored : store ref = some { commit := witness, encoded := .manifest blocks }) :
    ∀ fuel ≤ blocks.length + 1, resolveRequest store fuel ref witness = none := by
  intro fuel hLe
  cases fuel with
  | zero => rfl
  | succ m =>
      have hNone : resolveBlocks store m blocks = none :=
        resolveBlocks_insufficient_fuel store blocks m (by omega)
      simp [resolveRequest, resolveBytes, hStored, hNone]

/-- The durable `RenderedRequest` collection as a partial map from capture key
to canonical request. -/
abbrev Store := CaptureKey → Option CanonicalRequest

namespace Store

def empty : Store := fun _ => none

def bind (s : Store) (k : CaptureKey) (r : CanonicalRequest) : Store :=
  fun probe => if probe = k then some r else s probe

@[simp] theorem bind_self (s : Store) (k : CaptureKey) (r : CanonicalRequest) :
    bind s k r k = some r := by
  simp [bind]

@[simp] theorem bind_other (s : Store) (k probe : CaptureKey) (r : CanonicalRequest)
    (h : probe ≠ k) : bind s k r probe = s probe := by
  simp [bind, h]

@[simp] theorem empty_lookup (k : CaptureKey) : empty k = none := rfl

end Store

inductive CaptureOutcome where
  | fresh
  | idempotent
  | rejected
  deriving DecidableEq, Repr

namespace CaptureOutcome

def toContract : CaptureOutcome → String
  | .fresh => "fresh"
  | .idempotent => "idempotent"
  | .rejected => "rejected"

def durable : CaptureOutcome → Bool
  | .fresh => true
  | .idempotent => true
  | .rejected => false

end CaptureOutcome

def capture (s : Store) (k : CaptureKey) (r : CanonicalRequest) :
    CaptureOutcome × Store :=
  match s k with
  | none => (.fresh, Store.bind s k r)
  | some stored => if stored = r then (.idempotent, s) else (.rejected, s)

def captureEncoded (artifacts : ArtifactStore) (fuel ref witness : Nat)
    (s : Store) (k : CaptureKey) : Option (CaptureOutcome × Store) := do
  let request ← resolveRequest artifacts fuel ref witness
  some (capture s k request)

theorem encoded_decode_failure_blocks_capture (artifacts : ArtifactStore)
    (fuel ref witness : Nat) (s : Store) (k : CaptureKey)
    (h : resolveRequest artifacts fuel ref witness = none) :
    captureEncoded artifacts fuel ref witness s k = none := by
  simp [captureEncoded, h]

theorem manifest_resolution_failure_blocks_capture (artifacts : ArtifactStore)
    (fuel ref witness : Nat) (blocks : List (ArtifactRef × FieldCommitWitness))
    (s : Store) (k : CaptureKey)
    (hStored : artifacts ref = some { commit := witness, encoded := .manifest blocks })
    (hUnresolvable : ∀ blockRef blockWitness,
        (blockRef, blockWitness) ∈ blocks →
        ∀ encoded, artifacts blockRef ≠ some { commit := blockWitness, encoded }) :
    captureEncoded artifacts fuel ref witness s k = none := by
  refine encoded_decode_failure_blocks_capture artifacts fuel ref witness s k ?_
  cases blocks with
  | nil =>
      cases fuel with
      | zero => rfl
      | succ m =>
          cases m with
          | zero => simp [resolveRequest, resolveBytes, hStored, resolveBlocks]
          | succ m' =>
              simp [resolveRequest, resolveBytes, hStored, resolveBlocks, decodeCanonical]
  | cons entry rest =>
      obtain ⟨blockRef, blockWitness⟩ := entry
      cases fuel with
      | zero => rfl
      | succ m =>
          have hNone : resolveBlocks artifacts m ((blockRef, blockWitness) :: rest) = none :=
            resolveBlocks_unresolvable artifacts ((blockRef, blockWitness) :: rest) m
              blockRef blockWitness (by simp) (hUnresolvable blockRef blockWitness (by simp))
          simp [resolveRequest, resolveBytes, hStored, hNone]

theorem capture_fresh (s : Store) (k : CaptureKey) (r : CanonicalRequest)
    (h : s k = none) :
    capture s k r = (.fresh, Store.bind s k r) := by
  simp [capture, h]

theorem capture_idempotent (s : Store) (k : CaptureKey) (r : CanonicalRequest)
    (h : s k = some r) :
    capture s k r = (.idempotent, s) := by
  simp [capture, h]

theorem capture_rejects_rebinding (s : Store) (k : CaptureKey)
    (stored r : CanonicalRequest) (h : s k = some stored) (h_conflict : stored ≠ r) :
    capture s k r = (.rejected, s) := by
  simp [capture, h, h_conflict]

theorem capture_outcome_classified (s : Store) (k : CaptureKey) (r : CanonicalRequest) :
    ((capture s k r).1 = .fresh ↔ s k = none) ∧
      ((capture s k r).1 = .idempotent ↔ s k = some r) ∧
      ((capture s k r).1 = .rejected ↔ ∃ stored, s k = some stored ∧ stored ≠ r) := by
  cases h : s k with
  | none => simp [capture, h]
  | some stored =>
      by_cases h_eq : stored = r
      · subst h_eq; simp [capture, h]
      · simp [capture, h, h_eq]

theorem capture_durable_iff (s : Store) (k : CaptureKey) (r : CanonicalRequest) :
    (capture s k r).1.durable = true ↔ (capture s k r).2 k = some r := by
  cases h : s k with
  | none => simp [capture, h, CaptureOutcome.durable]
  | some stored =>
      by_cases h_eq : stored = r
      · subst h_eq; simp [capture, h, CaptureOutcome.durable]
      · simp [capture, h, h_eq, CaptureOutcome.durable]

theorem resolved_capture_durable_iff (artifacts : ArtifactStore) (fuel ref witness : Nat)
    (request : CanonicalRequest)
    (hDecode : resolveRequest artifacts fuel ref witness = some request)
    (s : Store) (k : CaptureKey) :
    ∃ result, captureEncoded artifacts fuel ref witness s k = some result ∧
      (result.1.durable = true ↔ result.2 k = some request) := by
  refine ⟨capture s k request, ?_, capture_durable_iff s k request⟩
  simp [captureEncoded, hDecode]

theorem capture_preserves_bindings (s : Store) (k : CaptureKey) (r : CanonicalRequest)
    {probe : CaptureKey} {stored : CanonicalRequest} (h : s probe = some stored) :
    (capture s k r).2 probe = some stored := by
  cases h_lookup : s k with
  | none =>
      have h_ne : probe ≠ k := by
        intro h_eq
        subst h_eq
        rw [h_lookup] at h
        exact Option.noConfusion h
      simp [capture, h_lookup, Store.bind, h_ne, h]
  | some other =>
      by_cases h_eq : other = r
      · simp [capture, h_lookup, h_eq, h]
      · simp [capture, h_lookup, h_eq, h]

theorem capture_at_other_key_preserves (s : Store) (k probe : CaptureKey)
    (r : CanonicalRequest) (h : probe ≠ k) :
    (capture s k r).2 probe = s probe := by
  cases h_lookup : s k with
  | none => simp [capture, h_lookup, Store.bind, h]
  | some other =>
      by_cases h_eq : other = r
      · simp [capture, h_lookup, h_eq]
      · simp [capture, h_lookup, h_eq]

theorem attempt_distinguishes_facts (k : CaptureKey) {a b : Nat} (h : a ≠ b) :
    ({ k with attempt := a } : CaptureKey) ≠ { k with attempt := b } := by
  intro h_eq
  exact h (congrArg CaptureKey.attempt h_eq)

inductive Stage where
  | assembled
  | durablyCaptured
  | sent
  deriving DecidableEq, Repr

namespace Stage

def toContract : Stage → String
  | .assembled => "assembled"
  | .durablyCaptured => "durablyCaptured"
  | .sent => "sent"

def all : List Stage := [.assembled, .durablyCaptured, .sent]

theorem all_complete (stage : Stage) : stage ∈ all := by
  cases stage <;> simp [all]

end Stage

structure Machine where
  store : Store
  stage : Stage
  key : CaptureKey
  request : CanonicalRequest

inductive Step : Machine → Machine → Prop where
  | captureFresh {pre post : Machine}
      (h_stage : pre.stage = .assembled)
      (h_unbound : pre.store pre.key = none)
      (h_post : post =
        { pre with stage := .durablyCaptured
                 , store := Store.bind pre.store pre.key pre.request })
      : Step pre post
  | captureIdempotent {pre post : Machine}
      (h_stage : pre.stage = .assembled)
      (h_bound : pre.store pre.key = some pre.request)
      (h_post : post = { pre with stage := .durablyCaptured })
      : Step pre post
  | captureRejected {pre post : Machine} (stored : CanonicalRequest)
      (h_stage : pre.stage = .assembled)
      (h_bound : pre.store pre.key = some stored)
      (h_conflict : stored ≠ pre.request)
      (h_post : post = pre)
      : Step pre post
  | send {pre post : Machine}
      (h_stage : pre.stage = .durablyCaptured)
      (h_durable : pre.store pre.key = some pre.request)
      (h_post : post = { pre with stage := .sent })
      : Step pre post
  | concurrentCapture {pre post : Machine} (k : CaptureKey) (r : CanonicalRequest)
      (h_unbound : pre.store k = none)
      (h_post : post = { pre with store := Store.bind pre.store k r })
      : Step pre post

inductive Trace : Machine → Machine → Prop where
  | refl {m : Machine} : Trace m m
  | step {a b c : Machine} : Step a b → Trace b c → Trace a c

namespace Step

theorem preserves_identity {pre post : Machine} (h : Step pre post) :
    post.key = pre.key ∧ post.request = pre.request := by
  cases h with
  | captureFresh _ _ h_post => subst h_post; exact ⟨rfl, rfl⟩
  | captureIdempotent _ _ h_post => subst h_post; exact ⟨rfl, rfl⟩
  | captureRejected _ _ _ _ h_post => subst h_post; exact ⟨rfl, rfl⟩
  | send _ _ h_post => subst h_post; exact ⟨rfl, rfl⟩
  | concurrentCapture _ _ _ h_post => subst h_post; exact ⟨rfl, rfl⟩

theorem store_monotone {pre post : Machine} (h : Step pre post)
    {probe : CaptureKey} {stored : CanonicalRequest}
    (h_bound : pre.store probe = some stored) :
    post.store probe = some stored := by
  cases h with
  | captureFresh _ h_unbound h_post =>
      subst h_post
      have h_ne : probe ≠ pre.key := by
        intro h_eq
        subst h_eq
        rw [h_unbound] at h_bound
        exact Option.noConfusion h_bound
      simpa [Store.bind, h_ne] using h_bound
  | captureIdempotent _ _ h_post => subst h_post; exact h_bound
  | captureRejected _ _ _ _ h_post => subst h_post; exact h_bound
  | send _ _ h_post => subst h_post; exact h_bound
  | concurrentCapture k _ h_unbound h_post =>
      subst h_post
      have h_ne : probe ≠ k := by
        intro h_eq
        subst h_eq
        rw [h_unbound] at h_bound
        exact Option.noConfusion h_bound
      simpa [Store.bind, h_ne] using h_bound

end Step

namespace Trace

theorem preserves_identity {a b : Machine} (h : Trace a b) :
    b.key = a.key ∧ b.request = a.request := by
  induction h with
  | refl => exact ⟨rfl, rfl⟩
  | step h_step _ ih =>
      obtain ⟨hk, hr⟩ := h_step.preserves_identity
      exact ⟨ih.1.trans hk, ih.2.trans hr⟩

theorem store_monotone {a b : Machine} (h : Trace a b) :
    ∀ {probe : CaptureKey} {stored : CanonicalRequest},
      a.store probe = some stored → b.store probe = some stored := by
  induction h with
  | refl => intro _ _ h_bound; exact h_bound
  | step h_step _ ih => intro _ _ h_bound; exact ih (h_step.store_monotone h_bound)

end Trace

def Machine.durable (m : Machine) : Prop :=
  m.store m.key = some m.request

def Machine.Invariant (m : Machine) : Prop :=
  m.stage ≠ Stage.assembled → m.durable

theorem Machine.invariant_of_assembled {m : Machine} (h : m.stage = Stage.assembled) :
    Machine.Invariant m := by
  intro h_ne
  exact absurd h h_ne

theorem Step.preserves_invariant {pre post : Machine}
    (h_inv : Machine.Invariant pre) (h : Step pre post) :
    Machine.Invariant post := by
  cases h with
  | captureFresh _ _ h_post =>
      subst h_post
      intro _
      simp [Machine.durable]
  | captureIdempotent _ h_bound h_post =>
      subst h_post
      intro _
      simpa [Machine.durable] using h_bound
  | captureRejected _ _ _ _ h_post => subst h_post; exact h_inv
  | send _ h_durable h_post =>
      subst h_post
      intro _
      simpa [Machine.durable] using h_durable
  | concurrentCapture k r h_unbound h_post =>
      subst h_post
      intro h_ne
      have h_pre : pre.store pre.key = some pre.request := h_inv h_ne
      have h_ne_key : pre.key ≠ k := by
        intro h_eq
        rw [h_eq, h_unbound] at h_pre
        exact Option.noConfusion h_pre
      simpa [Machine.durable, Store.bind, h_ne_key] using h_pre

theorem Trace.preserves_invariant {a b : Machine} (h : Trace a b)
    (h_inv : Machine.Invariant a) : Machine.Invariant b := by
  induction h with
  | refl => exact h_inv
  | step h_step _ ih => exact ih (h_step.preserves_invariant h_inv)

/-! ## The fenced properties -/

theorem sent_implies_durably_captured {init final : Machine}
    (h_start : init.stage = Stage.assembled)
    (h_trace : Trace init final)
    (h_sent : final.stage = Stage.sent) :
    final.store final.key = some final.request := by
  have h_inv := h_trace.preserves_invariant (Machine.invariant_of_assembled h_start)
  exact h_inv (by rw [h_sent]; decide)

def CaptureStep (pre post : Machine) : Prop :=
  pre.stage = Stage.assembled ∧
    post.stage = Stage.durablyCaptured ∧
    post.store post.key = some post.request ∧
    post.key = pre.key ∧
    post.request = pre.request

/-- **P1' — the send has a witness.** Stronger than P1: every trace from
`assembled` to `sent` factors through a successful capture step for this
`(key, request)`, so a fail-open sink that skipped the write could not produce
such a factorization. -/
theorem sent_requires_a_capture_step {init final : Machine} (h_trace : Trace init final) :
    init.stage = Stage.assembled → final.stage = Stage.sent →
      ∃ pre post, Trace init pre ∧ Step pre post ∧ CaptureStep pre post ∧ Trace post final := by
  induction h_trace with
  | @refl m =>
      intro h_start h_sent
      rw [h_start] at h_sent
      exact absurd h_sent (by decide)
  | @step a b c h_step _ ih =>
      intro h_start h_sent
      cases h_step with
      | captureFresh h_stage h_unbound h_post =>
          refine ⟨a, b, Trace.refl, Step.captureFresh h_stage h_unbound h_post, ?_, by assumption⟩
          subst h_post
          exact ⟨h_stage, rfl, by simp, rfl, rfl⟩
      | captureIdempotent h_stage h_bound h_post =>
          refine ⟨a, b, Trace.refl, Step.captureIdempotent h_stage h_bound h_post, ?_,
            by assumption⟩
          subst h_post
          exact ⟨h_stage, rfl, by simpa using h_bound, rfl, rfl⟩
      | captureRejected stored h_stage h_bound h_conflict h_post =>
          have h_b : b.stage = Stage.assembled := by subst h_post; exact h_start
          obtain ⟨pre, post, t_head, s, cs, t_tail⟩ := ih h_b h_sent
          exact ⟨pre, post,
            Trace.step (Step.captureRejected stored h_stage h_bound h_conflict h_post) t_head,
            s, cs, t_tail⟩
      | send h_stage _ _ =>
          rw [h_start] at h_stage
          exact absurd h_stage (by decide)
      | concurrentCapture k r h_unbound h_post =>
          have h_b : b.stage = Stage.assembled := by subst h_post; exact h_start
          obtain ⟨pre, post, t_head, s, cs, t_tail⟩ := ih h_b h_sent
          exact ⟨pre, post,
            Trace.step (Step.concurrentCapture k r h_unbound h_post) t_head, s, cs, t_tail⟩

/-- **P2 — one capture key names at most one canonical request**, stated over
the request *value*, not a digest, so no honest-writer assumption enters. -/
theorem capture_key_determines_request {init final : Machine} (h_trace : Trace init final)
    {k : CaptureKey} {r r' : CanonicalRequest}
    (h_init : init.store k = some r) (h_final : final.store k = some r') :
    r = r' := by
  have h_carried := h_trace.store_monotone h_init
  rw [h_carried] at h_final
  exact Option.some.inj h_final

theorem capture_failure_blocks_send {init final : Machine} {stored : CanonicalRequest}
    (h_start : init.stage = Stage.assembled)
    (h_bound : init.store init.key = some stored)
    (h_conflict : stored ≠ init.request)
    (h_trace : Trace init final) :
    final.stage ≠ Stage.sent := by
  intro h_sent
  have h_durable := sent_implies_durably_captured h_start h_trace h_sent
  obtain ⟨hk, hr⟩ := h_trace.preserves_identity
  rw [hk, hr] at h_durable
  have h_carried := h_trace.store_monotone h_bound
  rw [h_carried] at h_durable
  exact h_conflict (Option.some.inj h_durable)

structure Scenario where
  key : CaptureKey
  request : CanonicalRequest
  priorBinding : Option CanonicalRequest
  deriving Repr

namespace Scenario

def store (sc : Scenario) : Store :=
  match sc.priorBinding with
  | none => Store.empty
  | some stored => Store.bind Store.empty sc.key stored

@[simp] theorem store_lookup (sc : Scenario) : store sc sc.key = sc.priorBinding := by
  cases h : sc.priorBinding <;> simp [store, h]

def outcome (sc : Scenario) : CaptureOutcome :=
  (capture (store sc) sc.key sc.request).1

def postStore (sc : Scenario) : Store :=
  (capture (store sc) sc.key sc.request).2

def durableAfter (sc : Scenario) : Option CanonicalRequest :=
  postStore sc sc.key

def postStage (sc : Scenario) : Stage :=
  if (outcome sc).durable then .durablyCaptured else .assembled

def sendPermitted (sc : Scenario) : Bool :=
  decide (durableAfter sc = some sc.request)

def providerRequests (sc : Scenario) : Nat :=
  if sendPermitted sc then 1 else 0

def finalStage (sc : Scenario) : Stage :=
  if sendPermitted sc then .sent else postStage sc

def initialMachine (sc : Scenario) : Machine :=
  { store := store sc, stage := .assembled, key := sc.key, request := sc.request }

def finalMachine (sc : Scenario) : Machine :=
  { store := postStore sc, stage := finalStage sc, key := sc.key, request := sc.request }

theorem sendPermitted_iff_durable (sc : Scenario) :
    sendPermitted sc = true ↔ durableAfter sc = some sc.request := by
  simp [sendPermitted]

theorem rejected_blocks_send (sc : Scenario) (h : outcome sc = .rejected) :
    sendPermitted sc = false ∧ postStore sc = store sc := by
  have h_lookup : ∃ stored, store sc sc.key = some stored ∧ stored ≠ sc.request := by
    have := (capture_outcome_classified (store sc) sc.key sc.request).2.2
    exact this.mp h
  obtain ⟨stored, h_bound, h_conflict⟩ := h_lookup
  have h_capture := capture_rejects_rebinding (store sc) sc.key stored sc.request h_bound h_conflict
  constructor
  · simp [sendPermitted, durableAfter, postStore, h_capture, h_bound, h_conflict]
  · simp [postStore, h_capture]

theorem providerRequests_iff_sent (sc : Scenario) :
    providerRequests sc = 1 ↔ finalStage sc = Stage.sent := by
  cases h : sendPermitted sc <;> simp [providerRequests, finalStage, h, postStage] <;>
    cases h_outcome : (outcome sc).durable <;> simp

theorem trace_realizes (sc : Scenario) : Trace (initialMachine sc) (finalMachine sc) := by
  cases h_prior : sc.priorBinding with
  | none =>
      have h_unbound : store sc sc.key = none := by simp [h_prior]
      have h_capture : capture (store sc) sc.key sc.request =
          (.fresh, Store.bind (store sc) sc.key sc.request) :=
        capture_fresh _ _ _ h_unbound
      have h_post : postStore sc = Store.bind (store sc) sc.key sc.request := by
        simp [postStore, h_capture]
      have h_send : sendPermitted sc = true := by
        simp [sendPermitted, durableAfter, h_post]
      refine Trace.step (Step.captureFresh (pre := initialMachine sc)
        (post := { initialMachine sc with stage := .durablyCaptured
                                        , store := Store.bind (store sc) sc.key sc.request })
        rfl h_unbound rfl) ?_
      refine Trace.step (Step.send
        (post := { initialMachine sc with stage := .sent
                                        , store := Store.bind (store sc) sc.key sc.request })
        rfl (by simp [initialMachine]) rfl) ?_
      have : finalMachine sc =
          { initialMachine sc with stage := .sent
                                 , store := Store.bind (store sc) sc.key sc.request } := by
        simp [finalMachine, initialMachine, finalStage, h_send, h_post]
      rw [this]
      exact Trace.refl
  | some stored =>
      have h_bound : store sc sc.key = some stored := by simp [h_prior]
      by_cases h_eq : stored = sc.request
      · subst h_eq
        have h_capture : capture (store sc) sc.key sc.request = (.idempotent, store sc) :=
          capture_idempotent _ _ _ h_bound
        have h_post : postStore sc = store sc := by simp [postStore, h_capture]
        have h_send : sendPermitted sc = true := by
          simp [sendPermitted, durableAfter, h_post, h_bound]
        refine Trace.step (Step.captureIdempotent (pre := initialMachine sc)
          (post := { initialMachine sc with stage := .durablyCaptured })
          rfl (by simpa [initialMachine] using h_bound) rfl) ?_
        refine Trace.step (Step.send
          (post := { initialMachine sc with stage := .sent })
          rfl (by simpa [initialMachine] using h_bound) rfl) ?_
        have : finalMachine sc = { initialMachine sc with stage := .sent } := by
          simp [finalMachine, initialMachine, finalStage, h_send, h_post]
        rw [this]
        exact Trace.refl
      · have h_capture : capture (store sc) sc.key sc.request = (.rejected, store sc) :=
          capture_rejects_rebinding _ _ _ _ h_bound h_eq
        have h_post : postStore sc = store sc := by simp [postStore, h_capture]
        have h_outcome : outcome sc = .rejected := by simp [outcome, h_capture]
        have h_send : sendPermitted sc = false := (rejected_blocks_send sc h_outcome).1
        refine Trace.step (Step.captureRejected (pre := initialMachine sc)
          (post := initialMachine sc) stored rfl (by simpa [initialMachine] using h_bound)
          (by simpa [initialMachine] using h_eq) rfl) ?_
        have : finalMachine sc = initialMachine sc := by
          simp [finalMachine, initialMachine, finalStage, h_send, h_post, postStage,
            h_outcome, CaptureOutcome.durable]
        rw [this]
        exact Trace.refl

end Scenario

end RenderedCapture
