import Proofs.Configuration

namespace Configuration.CatalogRefresh

/-- One admitted credential scope of one owned backend. `payload` is the exact
canonical serialization of its validated advertised models. Source/configuration
and credential-scope admission precede this owner; refresh does not select a
model or grant access. Times are ordered instants, not host polling counters. -/
structure State where
  payload : String
  contentAt : Nat
  observedAt : Nat
  deriving DecidableEq, Repr

/-- A repeated payload advances successful-discovery freshness without changing
its retained content timestamp. The watermark must survive restart: otherwise a
late changed result can overwrite a newer successful identical discovery. -/
def observe (previous : Option State) (payload : String) (stamp : Nat) : State :=
  match previous with
  | none => ⟨payload, stamp, stamp⟩
  | some current =>
      if stamp < current.observedAt then current
      else if payload = current.payload then { current with observedAt := stamp }
      else ⟨payload, stamp, stamp⟩

def contentChanged (previous : Option State) (next : State) : Bool :=
  match previous with
  | none => true
  | some current => current.payload != next.payload || current.contentAt != next.contentAt

def watermarkChanged (previous : Option State) (next : State) : Bool :=
  match previous with
  | none => true
  | some current => current != next

theorem stale_preserves (current : State) (payload : String) (stamp : Nat)
    (h : stamp < current.observedAt) :
    observe (some current) payload stamp = current := by
  simp [observe, h]

theorem freshness_monotone (current : State) (payload : String) (stamp : Nat) :
    current.observedAt ≤ (observe (some current) payload stamp).observedAt := by
  by_cases stale : stamp < current.observedAt <;>
    by_cases same : payload = current.payload <;>
    simp [observe, stale, same] <;> omega

theorem identical_content_preserved (current : State) (stamp : Nat) :
    (observe (some current) current.payload stamp).payload = current.payload ∧
    (observe (some current) current.payload stamp).contentAt = current.contentAt := by
  by_cases stale : stamp < current.observedAt <;> simp [observe, stale]

theorem identical_content_never_written (current : State) (stamp : Nat) :
    contentChanged (some current) (observe (some current) current.payload stamp) = false := by
  rcases identical_content_preserved current stamp with ⟨hp, ht⟩
  simp [contentChanged, hp, ht]

theorem valid_content_time_preserved (current : State) (payload : String) (stamp : Nat)
    (h : current.contentAt ≤ current.observedAt) :
    (observe (some current) payload stamp).contentAt ≤
      (observe (some current) payload stamp).observedAt := by
  by_cases stale : stamp < current.observedAt <;>
    by_cases same : payload = current.payload <;>
    simp [observe, stale, same] <;> omega

end Configuration.CatalogRefresh
