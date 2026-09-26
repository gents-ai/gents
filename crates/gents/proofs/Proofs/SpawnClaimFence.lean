import Proofs.Basic
import Mathlib.Tactic.SplitIfs

/-!
# Unclaimed spawn fence (#1807)

A background `spawn_subagent` bridge names its child before any host has
materialized or claimed it. The parent observes the child only through a
replicated child row that corroborates the bridge's exact physical lineage, and
the host observes the bridge only through its replicated copy, so each side
acts on a possibly stale view of the other.

A spawn deadline (the unclaimed-spawn deadline or the bridge's ordinary tool
deadline, whichever settles the bridge first) gives up on a child the parent
has not observed. Giving up must not create a worker nobody supervises: the
same terminal write records a durable cancel intent on the bridge. Once the
bridge replicates to the host, the host refuses to materialize the child, the
claim gate refuses to claim it, and the cancel mirror latches the interrupt of
a child already running. Until the child is observed terminal the bridge's
acknowledgement stays pending, so the parent never treats a child that may
have already won its claim as stopped.
-/

namespace SpawnClaimFence

inductive Route where
  | samePrincipal
  | crossPrincipal
  deriving DecidableEq, Repr

inductive AwaitMode where
  | foreground
  | background
  deriving DecidableEq, Repr

/-- A background same-principal child is materialized by this runtime's own
    subagent source, so a queued child has not failed: its bridge keeps
    awaiting materialization with no fixed unclaimed failure. A cross-principal
    spawn waits on a peer that may never answer. A foreground spawn blocks its
    parent's turn, so on any route an unconfirmed child may not hold the
    parent past the bound (#1830); the parent's own deadline is not one. -/
def unclaimedDeadlineApplies : Route → AwaitMode → Bool
  | _, .foreground => true
  | .samePrincipal, .background => false
  | .crossPrincipal, .background => true

inductive Bridge where
  /-- Running; the parent has not observed the child. -/
  | awaiting
  /-- Running; the parent observed the child and cleared the unclaimed deadline. -/
  | linked
  /-- Failed `spawnUnclaimed` with a durable cancel intent. -/
  | abandoned
  /-- Timed out on its ordinary deadline with a durable cancel intent. -/
  | expired
  /-- Timed out after the parent observed its child. That child's stop belongs
      to the child-liveness owners, not to this fence. -/
  | settledObserved
  deriving DecidableEq, Repr

/-- A bridge settled without having observed its child. -/
def Bridge.fenced : Bridge → Bool
  | .abandoned | .expired => true
  | .awaiting | .linked | .settledObserved => false

inductive Child where
  | absent
  | pending
  | running
  | interrupted
  | finished
  deriving DecidableEq, Repr

def Child.terminal : Child → Bool
  | .interrupted | .finished => true
  | .absent | .pending | .running => false

structure World where
  bridge : Bridge
  child : Child
  /-- A child row corroborating the bridge's physical lineage has replicated
      to the parent's host. -/
  childVisible : Bool
  /-- Durable cancel intent recorded on the bridge. -/
  cancelIntent : Bool
  /-- The bridge's cancel intent has replicated to the child's host. -/
  hostSeesIntent : Bool
  /-- The child's host latched `interrupt_requested_at` on the child. -/
  interruptLatched : Bool
  /-- The bridge still awaits evidence that its child stopped. -/
  ackPending : Bool
  deriving DecidableEq, Repr

def World.initial : World :=
  { bridge := .awaiting, child := .absent, childVisible := false
  , cancelIntent := false, hostSeesIntent := false, interruptLatched := false
  , ackPending := false }

inductive Action where
  /-- Parent: the unclaimed-spawn deadline expired. -/
  | expire
  /-- Parent: the bridge's ordinary tool deadline expired. -/
  | deadline
  /-- Host: create the child against its current view of the bridge. -/
  | materialize
  /-- The child row replicates to the parent. -/
  | publishChild
  /-- The bridge replicates to the child's host. -/
  | replicateBridge
  /-- Host: latch the interrupt of a live child whose bridge carries an intent. -/
  | mirror
  /-- Host: a worker claims the child through the pre-claim gates. -/
  | claim
  /-- The running child stops: interrupted if latched, else it finishes. -/
  | stop
  /-- Parent: clear the pending acknowledgement once the child is terminal. -/
  | observeAck
  deriving DecidableEq, Repr

/-- Settle an awaiting bridge without an observed child: the terminal write
    carries the durable cancel intent and leaves the acknowledgement pending,
    because a missing child row is not proof that the child will never
    materialize or has not already won its claim. -/
def fence (w : World) (settled : Bridge) : World :=
  { w with bridge := settled, cancelIntent := true, ackPending := true }

/-- Unclaimed expiry. An observed child links the bridge and keeps running.
    Re-running expiry on a settled bridge is a no-op. -/
def expire (w : World) : World :=
  match w.bridge with
  | .awaiting => if w.childVisible then { w with bridge := .linked } else fence w .abandoned
  | .linked | .abandoned | .expired | .settledObserved => w

/-- Ordinary deadline expiry composes with the fence: whichever deadline fires
    first, an unobserved child is fenced in the same terminal write. -/
def deadline (w : World) : World :=
  match w.bridge with
  | .awaiting =>
      if w.childVisible then { w with bridge := .settledObserved } else fence w .expired
  | .linked => { w with bridge := .settledObserved }
  | .abandoned | .expired | .settledObserved => w

def step (w : World) : Action → World
  | .expire => expire w
  | .deadline => deadline w
  | .materialize =>
      if w.child == .absent && !w.hostSeesIntent then { w with child := .pending } else w
  | .publishChild => if w.child != .absent then { w with childVisible := true } else w
  | .replicateBridge => { w with hostSeesIntent := w.cancelIntent }
  | .mirror =>
      if w.hostSeesIntent && (w.child == .pending || w.child == .running) then
        { w with interruptLatched := true }
      else w
  | .claim =>
      if w.child == .pending then
        { w with child :=
            if w.interruptLatched || w.hostSeesIntent then .interrupted else .running }
      else w
  | .stop =>
      if w.child == .running then
        { w with child := if w.interruptLatched then .interrupted else .finished }
      else w
  | .observeAck =>
      if w.ackPending && w.child.terminal then { w with ackPending := false } else w

/-- Unclaimed expiry is only enabled on spawns that carry that deadline. A
    mode flip re-evaluates it, so settlement re-checks the bound on the row it
    writes: a bound cleared after selection makes the expiry a no-op. -/
def enabled (route : Route) (mode : AwaitMode) : Action → Bool
  | .expire => unclaimedDeadlineApplies route mode
  | _ => true

inductive Reachable (route : Route) (mode : AwaitMode) : World → Prop where
  | initial : Reachable route mode World.initial
  | step (w : World) (action : Action) :
      Reachable route mode w → enabled route mode action = true →
        Reachable route mode (step w action)

/-- An orphan is a live child of a fenced bridge whose stop is no longer
    awaited. -/
def orphan (w : World) : Bool :=
  w.bridge.fenced && !w.child.terminal && !(w.cancelIntent && w.ackPending)

/-- Invariant: a fenced bridge carries its cancel intent and keeps its
    acknowledgement pending until its child is terminal; the host only ever
    sees an intent the bridge carries; a latch only exists on a child row. -/
def fenceInvariant (w : World) : Bool :=
  (!w.hostSeesIntent || w.cancelIntent) &&
    (!w.interruptLatched || w.child != .absent) &&
    (!w.bridge.fenced || (w.cancelIntent && (w.ackPending || w.child.terminal)))

theorem expire_idempotent (w : World) : expire (expire w) = expire w := by
  rcases w with ⟨b, c, v, ci, hs, il, ap⟩
  cases b <;> cases v <;> simp [expire, fence]

theorem deadline_idempotent (w : World) : deadline (deadline w) = deadline w := by
  rcases w with ⟨b, c, v, ci, hs, il, ap⟩
  cases b <;> cases v <;> simp [deadline, fence]

/-- Both deadlines already expired at restart: the ordinary deadline settles
    the bridge and the later unclaimed expiry is a no-op. -/
theorem expire_after_deadline_is_noop (w : World) :
    expire (deadline w) = deadline w := by
  rcases w with ⟨b, c, v, ci, hs, il, ap⟩
  cases b <;> cases v <;> simp [expire, deadline, fence]

theorem step_preserves_fenceInvariant (w : World) (action : Action)
    (h : fenceInvariant w = true) : fenceInvariant (step w action) = true := by
  rcases w with ⟨b, c, v, ci, hs, il, ap⟩
  revert h
  cases action <;> cases b <;> cases c <;> cases v <;> cases ci <;> cases hs <;>
    cases il <;> cases ap <;> decide

theorem reachable_fenceInvariant (route : Route) (mode : AwaitMode) (w : World)
    (h : Reachable route mode w) :
    fenceInvariant w = true := by
  induction h with
  | initial => decide
  | step w action _ _ ih => exact step_preserves_fenceInvariant w action ih

/-- A late materialization or claim never leaves a live child of a fenced
    bridge unsupervised: its stop is still awaited. -/
theorem reachable_never_orphan (route : Route) (mode : AwaitMode) (w : World)
    (h : Reachable route mode w) : orphan w = false := by
  have hf := reachable_fenceInvariant route mode w h
  rcases w with ⟨b, c, v, ci, hs, il, ap⟩
  revert hf
  cases b <;> cases c <;> cases v <;> cases ci <;> cases hs <;> cases il <;>
    cases ap <;> decide

/-- A host that sees the bridge's intent refuses to materialize its child. -/
theorem materialize_after_visible_intent_is_refused (w : World)
    (h : w.hostSeesIntent = true) : (step w .materialize).child = w.child := by
  rcases w with ⟨b, c, v, ci, hs, il, ap⟩
  simp at h
  subst h
  simp [step]

/-- The claim gate reads the bridge's intent itself; it does not wait for the
    cancel mirror to latch the child. -/
theorem claim_after_visible_intent_is_refused (w : World)
    (hpending : w.child = .pending) (h : w.hostSeesIntent = true) :
    (step w .claim).child = .interrupted := by
  rcases w with ⟨b, c, v, ci, hs, il, ap⟩
  simp at hpending h
  subst hpending h
  simp [step]

/-- What a pending request row proves about the spawn bridge its lineage
    names. The logical child id is fixed by the parent before any row exists,
    so another row can reuse it and even name the bridge's physical document.
    Only a row whose reciprocal parent lineage matches the bridge receipt and
    that runs as the receipt's target principal is that bridge's child, the
    same resolution the cancel mirror and the acknowledgement observer use. -/
structure ClaimLineage where
  parentCorroborates : Bool
  targetCorroborates : Bool
  deriving DecidableEq, Repr

def ClaimLineage.bridgeChild (l : ClaimLineage) : Bool :=
  l.parentCorroborates && l.targetCorroborates

/-- The claim gate's reading of a bridge intent. The modeled `claim` step is
    this gate applied to the bridge's corroborated child; any other row is
    stopped only through its own interrupt latch. -/
def claimFencedByIntent (l : ClaimLineage) (hostSeesIntent : Bool) : Bool :=
  hostSeesIntent && l.bridgeChild

/-- A bridge intent refuses a claim exactly when the claimant is the bridge's
    child: a row with the wrong parent lineage or principal is never
    interrupted by it. -/
theorem claim_intent_reaches_only_bridge_child (l : ClaimLineage) (h : Bool) :
    claimFencedByIntent l h = true ↔ h = true ∧ l.bridgeChild = true := by
  cases h <;> simp [claimFencedByIntent]

theorem claim_intent_skips_wrong_parent (target h : Bool) :
    claimFencedByIntent ⟨false, target⟩ h = false := by
  cases h <;> cases target <;> rfl

theorem claim_intent_skips_wrong_target (parent h : Bool) :
    claimFencedByIntent ⟨parent, false⟩ h = false := by
  cases h <;> cases parent <;> rfl

/-- On the corroborated child the gate is exactly the modeled `claim` step's
    intent guard. -/
theorem bridge_child_claim_gate_matches_step (w : World)
    (hpending : w.child = .pending) (hlatch : w.interruptLatched = false) :
    ((step w .claim).child = .interrupted) ↔
      claimFencedByIntent ⟨true, true⟩ w.hostSeesIntent = true := by
  rcases w with ⟨b, c, v, ci, hs, il, ap⟩
  simp at hpending hlatch
  subst hpending hlatch
  cases hs <;> simp [step, claimFencedByIntent, ClaimLineage.bridgeChild]

/-- A fenced bridge's acknowledgement clears only on a terminal child. -/
theorem fenced_ack_requires_terminal_child (w : World) (h : w.child.terminal = false) :
    (step w .observeAck).ackPending = w.ackPending := by
  rcases w with ⟨b, c, v, ci, hs, il, ap⟩
  cases c <;> simp_all [step, Child.terminal]

theorem step_keeps_bridge (w : World) (action : Action)
    (h : action ≠ .expire) (h' : action ≠ .deadline) :
    (step w action).bridge = w.bridge := by
  cases action <;> simp at h h' <;> simp only [step] <;> (try split_ifs) <;> rfl

/-- A same-principal background spawn is never abandoned by the unclaimed
    deadline. -/
theorem same_principal_background_never_abandoned (w : World)
    (h : Reachable .samePrincipal .background w) : w.bridge ≠ .abandoned := by
  induction h with
  | initial => simp [World.initial]
  | step w action _ henabled ih =>
      by_cases hd : action = .deadline
      · subst hd
        rcases w with ⟨b, c, v, ci, hs, il, ap⟩
        simp at ih
        cases b <;> cases v <;> simp_all [step, deadline, fence]
      · have hne : action ≠ .expire := by
          intro hexp
          subst hexp
          simp [enabled, unclaimedDeadlineApplies] at henabled
        rw [step_keeps_bridge w action hne hd]
        exact ih

/-- Neither expiry treats a child that already won its claim as stopped. -/
theorem expiry_keeps_live_child_unsettled (w : World) (hawait : w.bridge = .awaiting) :
    (expire w).child = w.child ∧ (deadline w).child = w.child ∧
      ((expire w).bridge = .linked ∨ (expire w).ackPending = true) ∧
      ((deadline w).bridge = .settledObserved ∨ (deadline w).ackPending = true) := by
  rcases w with ⟨b, c, v, ci, hs, il, ap⟩
  simp at hawait
  subst hawait
  cases v <;> simp [expire, deadline, fence]

/-- Continuation from an already-reached world under a (possibly new) mode.
    A mode flip re-evaluates `unclaimedDeadlineApplies` for the new mode. -/
inductive ReachableFrom (route : Route) (mode : AwaitMode) (start : World) : World → Prop where
  | refl : ReachableFrom route mode start start
  | step (w : World) (action : Action) :
      ReachableFrom route mode start w → enabled route mode action = true →
        ReachableFrom route mode start (step w action)

theorem same_principal_background_continuation_never_abandons (start w : World)
    (hstart : start.bridge ≠ .abandoned)
    (h : ReachableFrom .samePrincipal .background start w) : w.bridge ≠ .abandoned := by
  induction h with
  | refl => exact hstart
  | step w action _ henabled ih =>
      by_cases hd : action = .deadline
      · subst hd
        rcases w with ⟨b, c, v, ci, hs, il, ap⟩
        simp at ih
        cases b <;> cases v <;> simp_all [step, deadline, fence]
      · have hne : action ≠ .expire := by
          intro hexp
          subst hexp
          simp [enabled, unclaimedDeadlineApplies] at henabled
        rw [step_keeps_bridge w action hne hd]
        exact ih

/-- A same-principal foreground spawn that is backgrounded before its bound
    abandoned it (explicitly, or by an interrupted parent retaining its awaited
    child) drops the bound with the mode flip, so it is never abandoned after. -/
theorem foreground_then_background_same_principal_never_abandoned (w0 w : World)
    (_h0 : Reachable .samePrincipal .foreground w0) (hnot : w0.bridge ≠ .abandoned)
    (h : ReachableFrom .samePrincipal .background w0 w) : w.bridge ≠ .abandoned :=
  same_principal_background_continuation_never_abandons w0 w hnot h

/-- A foreground spawn on any route carries the unclaimed bound: its parent's
    turn is never held by a child that was never confirmed (#1830). -/
theorem foreground_always_bounded (route : Route) :
    unclaimedDeadlineApplies route .foreground = true := by
  cases route <;> rfl

/-- Once the bound expires on an unconfirmed foreground child the bridge is
    settled, so the parent's blocked turn receives a result. -/
theorem foreground_expiry_settles_unconfirmed (route : Route) (w : World)
    (hawait : w.bridge = .awaiting) (hunseen : w.childVisible = false) :
    enabled route .foreground .expire = true ∧
      (step w .expire).bridge = .abandoned := by
  rcases w with ⟨b, c, v, ci, hs, il, ap⟩
  simp at hawait hunseen
  subst hawait hunseen
  cases route <;> simp [enabled, unclaimedDeadlineApplies, step, expire, fence]

end SpawnClaimFence
