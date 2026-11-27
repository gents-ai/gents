namespace Identity.DetachedGrantFence

/-! Detached relationship-grant completion against concurrent owner revocation.

A transaction that creates a document or queues an owner-staged change commits
before its detached ACP1 relationship grants run
(`ConfigAccess::complete_relationship_grants`). While that completion is in
flight, or before a caller replays an incomplete grant suffix, an authorized
owner may revoke the relationship. Without a fence, the late completion
re-establishes access after the revocation.

Each node holds a monotonic revocation fence serialized across detached grant
completions and owner revocations (`tla/DetachedGrantRevocationFence.tla`). A
succeeded revocation advances the node epoch and records that epoch for the
revoked tuple; a denied or failed revocation leaves the fence unchanged. A
detached completion observes whether any revocation succeeded for that tuple
after its baseline epoch and never re-grants a revoked tuple. An explicit
direct grant by an authorized owner is not blocked by an earlier revocation. -/

/-- Whether a relationship grant is a detached post-commit completion or an
explicit direct grant. -/
inductive GrantOrigin where
  | detached
  | explicit
  deriving DecidableEq, Repr

/-- What the relationship-grant owner decides under the revocation fence. -/
inductive CompletionDecision where
  /-- Issue the native relationship grant while holding the fence gate. -/
  | grant
  /-- An owner revocation succeeded since the detached grant was queued; leave
  the relationship revoked without calling native add. -/
  | skipRevoked
  /-- The authenticated actor does not match the grant's creator. -/
  | refuseActorMismatch
  deriving DecidableEq, Repr

/-- Decision: a detached completion grants only when the authenticated actor
matches the creator and no revocation of that tuple succeeded since its
baseline epoch. An explicit direct grant by the authorized actor is not blocked
by an earlier revocation.

External premise: DefraDB applies each native relationship add or delete
atomically; the per-node fence gate serializes detached completion and owner
revocation on that node so a revocation cannot commit between the check and the
add (`tla/DetachedGrantRevocationFence.tla`). -/
def decideGrant (origin : GrantOrigin) (creatorMatches revokedSinceQueue : Bool) :
    CompletionDecision :=
  if !creatorMatches then .refuseActorMismatch
  else match origin with
    | .explicit => .grant
    | .detached => if revokedSinceQueue then .skipRevoked else .grant

/-- Per-node revocation fence state for one relationship tuple. -/
structure FenceState where
  epoch : Nat := 0
  tupleRevocationEpoch : Nat := 0
  deriving DecidableEq, Repr

/-- Whether a tuple revoked at `tupleRevocationEpoch` was revoked after a
detached grant whose baseline epoch is `grantBaselineEpoch`. -/
def isRevokedSince (tupleRevocationEpoch grantBaselineEpoch : Nat) : Bool :=
  decide (grantBaselineEpoch < tupleRevocationEpoch)

/-- Only a succeeded native revocation advances the fence epoch for the tuple;
a denied or failed revocation leaves the fence unchanged so it cannot block a
legitimate detached completion. -/
def recordRevocation (fence : FenceState) (revocationSucceeded : Bool) : FenceState :=
  if revocationSucceeded then
    let next := fence.epoch + 1
    { epoch := next, tupleRevocationEpoch := next }
  else fence

def decideAgainstFence (fence : FenceState) (origin : GrantOrigin)
    (creatorMatches : Bool) (grantBaselineEpoch : Nat) : CompletionDecision :=
  decideGrant origin creatorMatches (isRevokedSince fence.tupleRevocationEpoch grantBaselineEpoch)

theorem detached_completion_never_grants_after_revocation (creatorMatches : Bool) :
    decideGrant .detached creatorMatches true ≠ .grant := by
  cases creatorMatches <;> decide

theorem actor_mismatch_always_refuses (origin : GrantOrigin) (revokedSinceQueue : Bool) :
    decideGrant origin false revokedSinceQueue = .refuseActorMismatch := by
  cases origin <;> cases revokedSinceQueue <;> rfl

theorem explicit_grant_by_creator_succeeds_after_earlier_revocation :
    decideGrant .explicit true true = .grant := rfl

theorem every_decision_is_classified (origin : GrantOrigin)
    (creatorMatches revokedSinceQueue : Bool) :
    decideGrant origin creatorMatches revokedSinceQueue = .grant ∨
      decideGrant origin creatorMatches revokedSinceQueue = .skipRevoked ∨
      decideGrant origin creatorMatches revokedSinceQueue = .refuseActorMismatch := by
  cases origin <;> cases creatorMatches <;> cases revokedSinceQueue <;> decide

theorem succeeded_revocation_fences_in_flight_detached_grant (fence : FenceState)
    (baseline : Nat) (h : baseline ≤ fence.epoch) :
    decideAgainstFence (recordRevocation fence true) .detached true baseline = .skipRevoked := by
  simp only [decideAgainstFence, recordRevocation, decideGrant, isRevokedSince,
    Bool.not_true, Bool.false_eq_true, ↓reduceIte]
  have hlt : baseline < fence.epoch + 1 := Nat.lt_succ_of_le h
  simp [hlt]

theorem failed_revocation_does_not_fence_clear_detached_grant (fence : FenceState)
    (baseline : Nat) (h : fence.tupleRevocationEpoch ≤ baseline) :
    decideAgainstFence (recordRevocation fence false) .detached true baseline = .grant := by
  simp only [decideAgainstFence, recordRevocation, decideGrant, isRevokedSince,
    Bool.not_true, Bool.false_eq_true, ↓reduceIte]
  have hnot : ¬ baseline < fence.tupleRevocationEpoch := Nat.not_lt_of_ge h
  simp [hnot]

theorem fresh_detached_grant_after_revocation_proceeds (fence : FenceState) :
    let afterRevoke := recordRevocation fence true
    decideAgainstFence afterRevoke .detached true afterRevoke.epoch = .grant := by
  simp only [decideAgainstFence, recordRevocation, decideGrant, isRevokedSince,
    Bool.not_true, Bool.false_eq_true, ↓reduceIte, Nat.lt_irrefl, decide_false]

end Identity.DetachedGrantFence
