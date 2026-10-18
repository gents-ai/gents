import Proofs.Triggers.Types

namespace Triggers.Durable

structure ClaimObservation where
  document : String
  owner : String
  session : String
  trigger : String := ""
  serial : Bool := false
  receipt : Bool := false
  arrival : Option Nat := none
  running : Bool := false
  terminal : Bool := false
  deriving DecidableEq, Repr

def claimConflict (candidate other : ClaimObservation) : Bool :=
  candidate.owner == other.owner &&
    (candidate.session == other.session ||
      (candidate.serial && candidate.receipt && other.receipt &&
        candidate.trigger == other.trigger))

def observedPrecedes (candidate other : ClaimObservation) : Bool :=
  match candidate.arrival, other.arrival with
  | some _, none => true
  | some next, some prior => prior < next
  | none, _ => false

def observedClaimAllowed (candidate : ClaimObservation) (rows : List ClaimObservation) : Bool :=
  !candidate.running && !candidate.terminal &&
  !(candidate.receipt && candidate.arrival.isNone) &&
  !rows.any (fun other => other.document != candidate.document &&
    !other.terminal && claimConflict candidate other &&
    ((other.receipt && other.arrival.isNone) || other.running ||
      observedPrecedes candidate other))

theorem receipt_without_arrival_cannot_claim (candidate : ClaimObservation)
    (hReceipt : candidate.receipt = true) (hArrival : candidate.arrival = none)
    (rows : List ClaimObservation) : observedClaimAllowed candidate rows = false := by
  simp [observedClaimAllowed, hReceipt, hArrival]

theorem historical_pending_has_no_order (candidate other : ClaimObservation)
    (hArrival : candidate.arrival = none) : observedPrecedes candidate other = false := by
  simp [observedPrecedes, hArrival]


theorem running_conflict_prevents_claim (candidate other : ClaimObservation)
    (rows : List ClaimObservation) (hm : other ∈ rows)
    (hd : other.document ≠ candidate.document)
    (hc : claimConflict candidate other = true)
    (hr : other.running = true) (ht : other.terminal = false) :
    observedClaimAllowed candidate rows = false := by
  have hb : rows.any (fun row => row.document != candidate.document &&
      !row.terminal && claimConflict candidate row &&
      ((row.receipt && row.arrival.isNone) || row.running ||
        observedPrecedes candidate row)) = true := by
    apply List.any_eq_true.mpr
    exact ⟨other, hm, by simp [hd, hc, hr, ht]⟩
  simp [observedClaimAllowed, hb]

theorem earlier_conflict_prevents_claim (candidate other : ClaimObservation)
    (rows : List ClaimObservation) (hm : other ∈ rows)
    (hd : other.document ≠ candidate.document)
    (hc : claimConflict candidate other = true)
    (hp : observedPrecedes candidate other = true)
    (ht : other.terminal = false) :
    observedClaimAllowed candidate rows = false := by
  have hb : rows.any (fun row => row.document != candidate.document &&
      !row.terminal && claimConflict candidate row &&
      ((row.receipt && row.arrival.isNone) || row.running ||
        observedPrecedes candidate row)) = true := by
    apply List.any_eq_true.mpr
    exact ⟨other, hm, by simp [hd, hc, hp, ht]⟩
  simp [observedClaimAllowed, hb]

theorem queued_serial_fifo (candidate other : ClaimObservation)
    (rows : List ClaimObservation) (hm : other ∈ rows)
    (hd : other.document ≠ candidate.document)
    (ho : candidate.owner = other.owner) (hg : candidate.trigger = other.trigger)
    (hs : candidate.serial = true) (hc : candidate.receipt = true)
    (hr : other.receipt = true) (ht : other.terminal = false)
    (next prior : Nat) (hn : candidate.arrival = some next)
    (hp : other.arrival = some prior) (he : prior < next) :
    observedClaimAllowed candidate rows = false := by
  apply earlier_conflict_prevents_claim candidate other rows hm hd
  · simp [claimConflict, ho, hg, hs, hc, hr]
  · simp [observedPrecedes, hn, hp, he]
  · exact ht

theorem same_session_running_exclusion (candidate other : ClaimObservation)
    (rows : List ClaimObservation) (hm : other ∈ rows)
    (hd : other.document ≠ candidate.document)
    (ho : candidate.owner = other.owner) (hs : candidate.session = other.session)
    (hr : other.running = true) (ht : other.terminal = false) :
    observedClaimAllowed candidate rows = false := by
  apply running_conflict_prevents_claim candidate other rows hm hd
  · simp [claimConflict, ho, hs]
  · exact hr
  · exact ht

end Triggers.Durable
