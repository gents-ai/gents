import Proofs.P2P.Enrollment.RenewalNotices
import Proofs.Conformance.Family

namespace Conformance.EnrollmentRenewalNotices

open Lean Enrollment.RenewalNotices

deriving instance Lean.ToJson for LeaseState, Observation, Trace

def observations : List Observation :=
  { active := false, sequence := 0, state := .current } ::
    ([1, 2] : List Nat).flatMap fun sequence =>
      [LeaseState.current, .renewalDue, .expired, .invalid].map fun state =>
        { active := true, sequence, state }

def scenario : Trace :=
  { steps :=
      [ { active := true, sequence := 1, state := .current },
        { active := true, sequence := 1, state := .renewalDue },
        { active := true, sequence := 1, state := .renewalDue },
        { active := true, sequence := 1, state := .expired },
        { active := true, sequence := 1, state := .expired },
        { active := true, sequence := 2, state := .current },
        { active := true, sequence := 2, state := .renewalDue } ] }

def traces : List Trace :=
  observations.flatMap (fun a => observations.flatMap fun b =>
    observations.map fun c => ({ steps := [a, b, c] } : Trace)) ++ [scenario]

@[conformance_family]
def trace : Family Trace (List (Option LeaseState)) where
  inputs := traces
  model := fun t => Enrollment.RenewalNotices.notices [] t.steps

end Conformance.EnrollmentRenewalNotices
