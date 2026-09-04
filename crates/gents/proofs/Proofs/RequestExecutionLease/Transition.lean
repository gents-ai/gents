import Proofs.RequestExecutionLease.State

namespace RequestExecutionLease

/-!
`persistProgress` is the sole lease-renewal transition.  Its constructors name
the three durable semantic sources accepted by the runtime contract.  Socket
traffic and no-ops deliberately remain legal observations even after the
deadline, but return the exact same world, so they cannot defer `expire`.

Both live completion and recovery failure call the one `terminalize` helper;
there is no response-only or request-only terminal transition.
-/

inductive Action (Generation : Type) where
  | claim (generation : Generation) (deadline : Time)
  | begin (generation : Generation)
  | persistProgress (generation : Generation) (kind : ProgressKind) (newDeadline : Time)
  | socketTraffic (generation : Generation)
  | noOp (generation : Generation)
  | advanceTime (now : Time)
  | drop (generation : Generation)
  | expire (generation : Generation)
  | recover (expected : Generation) (fresh : Generation) (deadline : Time)
  | finalize (generation : Generation) (outcome : Outcome)
  | revoke (expected : Generation) (expectedDeadline : Time) (expectedProgress : Nat)
      (fresh : Generation) (outcome : Outcome)
  | recoverAndFail (expected : Generation) (fresh : Generation)
  deriving DecidableEq, Repr

def step? {Generation : Type} [DecidableEq Generation]
    (pre : World Generation) : Action Generation → Option (World Generation)
  | .claim generation deadline =>
      match pre.lease with
      | .vacant =>
          if pre.request = .pending ∧ pre.response = .absent ∧
              fresh pre generation ∧ pre.now < deadline then
            some
              { pre with
                request := .claimed
                lease := .active generation deadline
                usedGenerations := generation :: pre.usedGenerations }
          else
            none
      | _ => none
  | .begin generation =>
      match pre.lease with
      | .active owner deadline =>
          if owner = generation ∧ pre.now ≤ deadline ∧
              pre.request = .claimed ∧ pre.response = .absent then
            some { pre with request := .processing, response := .streaming }
          else
            none
      | _ => none
  | .persistProgress generation _ newDeadline =>
      match pre.lease with
      | .active owner deadline =>
          if owner = generation ∧ pre.now ≤ deadline ∧ deadline < newDeadline ∧
              pre.request = .processing ∧ pre.response = .streaming then
            some
              { pre with
                lease := .active owner newDeadline
                progressSeq := pre.progressSeq + 1 }
          else
            none
      | _ => none
  | .socketTraffic generation =>
      match pre.lease with
      | .active owner _ => if owner = generation then some pre else none
      | _ => none
  | .noOp generation =>
      match pre.lease with
      | .active owner _ => if owner = generation then some pre else none
      | _ => none
  | .advanceTime now =>
      if pre.now ≤ now then some { pre with now := now } else none
  | .drop generation =>
      match pre.lease with
      | .active owner _ =>
          if owner = generation then
            some { pre with lease := .recoverable owner }
          else
            none
      | _ => none
  | .expire generation =>
      match pre.lease with
      | .active owner deadline =>
          if owner = generation ∧ deadline < pre.now then
            some { pre with lease := .recoverable owner }
          else
            none
      | _ => none
  | .recover expected generation deadline =>
      match pre.lease with
      | .recoverable owner =>
          if owner = expected ∧ fresh pre generation ∧ pre.now < deadline then
            some
              { pre with
                lease := .active generation deadline
                usedGenerations := generation :: pre.usedGenerations }
          else
            none
      | _ => none
  | .finalize generation outcome =>
      match pre.lease with
      | .active owner deadline =>
          if owner = generation ∧ pre.now ≤ deadline ∧ canFinalize pre outcome ∧
              pre.continuationCount = 0 ∧ pre.tokenChargeCount = 0 then
            some (terminalize pre owner outcome)
          else
            none
      | _ => none
  | .revoke expected expectedDeadline expectedProgress generation outcome =>
      match pre.lease with
      | .active owner deadline =>
          if owner = expected ∧ deadline = expectedDeadline ∧
              pre.progressSeq = expectedProgress ∧ fresh pre generation ∧
              (outcome = .dead ∨ outcome = .superseded) ∧ canFinalize pre outcome ∧
              pre.continuationCount = 0 ∧ pre.tokenChargeCount = 0 then
            some (terminalize
              { pre with usedGenerations := generation :: pre.usedGenerations }
              generation outcome)
          else none
      | _ => none
  | .recoverAndFail expected generation =>
      match pre.lease with
      | .recoverable owner =>
          if owner = expected ∧ fresh pre generation ∧
              canFinalize pre .failed ∧ pre.continuationCount = 0 ∧
              pre.tokenChargeCount = 0 then
            some
              (terminalize
                { pre with usedGenerations := generation :: pre.usedGenerations }
                generation .failed)
          else
            none
      | _ => none

def replay? {Generation : Type} [DecidableEq Generation] :
    World Generation → List (Action Generation) → Option (World Generation)
  | world, [] => some world
  | world, action :: rest =>
      match step? world action with
      | none => none
      | some next => replay? next rest

end RequestExecutionLease
