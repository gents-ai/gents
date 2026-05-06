import Proofs.ClientShell.Types

/-!
# Client Shell Submission Gate

Transport-independent follow-up submission predicates.
-/

/-! ## Submission gate

Client availability is a separate input from transport health. A
"healthy transport, offline client" is a legal configuration (e.g.
during a voluntary restart) and must still block submission. -/

structure SubmitContext where
  clientAvailable   : Bool
  composerNonEmpty  : Bool
  requestedBehavior : Option BehaviorId
  deriving Repr

/-- Behavior-mismatch check: if the composer has an override that
    differs from the session's persisted behavior, block. If either
    side is absent, no mismatch is reported. -/
def behaviorMismatch
    (store : LocalStore) (sid : SessionId)
    (requested : Option BehaviorId) : Bool :=
  match requested, (store.find sid).bind (·.behaviorId) with
  | some r, some e => decide (r ≠ e)
  | _, _           => false

/-- Whether the selected session is trustworthy enough for follow-up
    submission. Transport-independent by signature. -/
def trustworthyForFollowUp
    (s : ShellState) (store : LocalStore)
    (requestedBehavior : Option BehaviorId) : Bool :=
  match s.selection.session with
  | none     => true
  | some sid =>
    match store.find sid with
    | none     => false
    | some obs =>
      let tipCoherent :=
        match obs.latestObservedRequest, obs.latestTurn with
        | some _, some _ => true
        | none,   none   => true
        | _,      _      => false
      let noMismatch := ¬ behaviorMismatch store sid requestedBehavior
      tipCoherent && noMismatch

/-- Submission safety predicate. Transport-independent by signature —
    only `SubmitContext.clientAvailable` represents "can we even emit
    a mutation right now." -/
def canSubmit
    (s : ShellState) (store : LocalStore) (ctx : SubmitContext) : Bool :=
  if ¬ ctx.clientAvailable then false
  else if s.selection.agent.isNone then false
  else if ¬ ctx.composerNonEmpty then false
  else
    match s.workflow with
    | .creating _ | .submitting _ _ | .awaiting _ _ | .blocked _ => false
    | .idle =>
      match s.selection.session with
      | none     => true  -- new conversation: no session to validate
      | some sid =>
        match store.find sid with
        | none     => false
        | some obs =>
          let tipTerminalOrUnstarted :=
            match obs.latestObservedRequest, obs.latestTurn with
            | some _, some t => t.isTerminal
            | none,   none   => true
            | _,      _      => false
          let noMismatch := ¬ behaviorMismatch store sid ctx.requestedBehavior
          tipTerminalOrUnstarted && noMismatch
