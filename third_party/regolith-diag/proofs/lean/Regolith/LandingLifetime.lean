/- A completed read's caller can resume within a finite window of other I/O polls.
   The queue retains its landing up to a finite live limit. Once no caller
   exists, the old grace-period rule reclaims it. LandingLifetime.tla checks
   interleaved clones, drops, polling and resumption; these laws hold at every
   age and grace period. Correspondence: keep is Ledger::settle's expiry guard,
   live is a caller Arc beyond Ledger's ownership, age is its poll count. -/
namespace Regolith.LandingLifetime

-- An active caller or the grace period protects a completed wait.
def keep (live : Bool) (age grace limit : Nat) : Bool :=
  -- Caller ownership extends grace only within the live limit.
  decide (age ≤ limit) && (live || decide (age ≤ grace))

-- A live caller keeps its landing within the finite window.
theorem live_retained (age grace limit : Nat) (young : age ≤ limit) :
    -- The live landing remains protected.
    keep true age grace limit = true := by
  -- The live branch decides the disjunction.
  simp [keep, young]

-- A cancelled wait past its grace period is reclaimable.
theorem abandoned_expires (age grace limit : Nat) (old : grace < age) :
    -- Its landing can be released.
    keep false age grace limit = false := by
  -- Natural-number order makes the grace test false.
  simp [keep, Nat.not_le_of_lt old]

-- A cancelled wait within its grace period is still retained.
theorem grace_retained (age grace limit : Nat) (young : age ≤ grace)
    -- The live window includes the abandoned grace period.
    (ordered : grace ≤ limit) :
    -- Its landing remains protected.
    keep false age grace limit = true := by
  -- The grace branch decides the disjunction.
  simp [keep, young, Nat.le_trans young ordered]

-- Even a forgotten live handle expires after the finite limit.
theorem live_expires (live : Bool) (age grace limit : Nat) (old : limit < age) :
    -- No ownership state overrides the limit.
    keep live age grace limit = false := by
  -- The age guard is false.
  simp [keep, Nat.not_le_of_lt old]

-- Another read can consume a retained wait only when its caller is gone.
def releaseCovered (live : Bool) : Bool := !live

-- Reading the same block elsewhere cannot release a live caller's hold.
theorem other_read_retains_live : releaseCovered true = false := by
  -- A live caller vetoes release independently of what another read found.
  rfl

-- End of the completed-wait lifetime laws.
end Regolith.LandingLifetime
