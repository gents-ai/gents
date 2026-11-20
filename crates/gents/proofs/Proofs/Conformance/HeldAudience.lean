import Proofs.Store.Identity.HeldAudience
import Proofs.Conformance.Family
import Proofs.Conformance.UserDelegation
import Proofs.Conformance.TransactionReplay

namespace Identity.HeldAudience

deriving instance Lean.ToJson for Held, Change, Refusal, Answer, Told, UnitStage, Recovery,
  Advertised, Reconciled

attribute [conformance_function] change resend refusalOf told resendUnit rereadBeforeRequestBound
  recovery reconcile

end Identity.HeldAudience

namespace Identity.HeldAudience

/-- A report as a case states it: a named audience is the fixed worker at
`deployment`. -/
structure ReportCase where
  reported : UserDelegation.Reported
  deployment : Nat
  providerTime : Option Nat
  deriving Lean.ToJson

def ReportCase.report (case : ReportCase) : Report where
  reading := match case.reported with
    | .absent => .absent
    | .named => .named ⟨"worker", case.deployment, none⟩
    | .unavailable => .unavailable
  providerTime := case.providerTime

/-- A signature as a case states it: by the saved home's key or another, over a
home status statement or another kind, the nonce and node it covers, and the
report it covers. -/
structure SignatureCase where
  byHome : Bool
  homeStatus : Bool
  nonce : Nat
  namesHome : Bool
  covers : ReportCase
  deriving Lean.ToJson

/-- A status read with nonce 1 from the saved home's address. -/
structure HomeStatusInput where
  namesHome : Bool
  report : ReportCase
  signature : Option SignatureCase
  deriving Lean.ToJson

structure ReportOutput where
  reported : UserDelegation.Reported
  deployment : Option Nat
  providerTime : Option Nat
  deriving Lean.ToJson

def node (home : Bool) : String := if home then "home" else "other"

def SignatureCase.signature (case : SignatureCase) : Signature :=
  ⟨node case.byHome,
    ⟨if case.homeStatus then .homeStatus else .other, case.nonce, node case.namesHome,
      case.covers.report⟩⟩

def ReportOutput.of (report : Report) : ReportOutput where
  reported := report.reported
  deployment := match report.reading with
    | .named audience => some audience.deployment
    | _ => none
  providerTime := report.providerTime

/-- The home's statuses signed as it signs them, for an audience, for the
node's own store and for an audience it could not read; a status with no
signature; one signed by another key; one signed for another nonce, the same
and another audience; one signed as another kind of statement; one whose
audience or clock was altered after signing; and another node's status signed
by its own key. -/
@[conformance_family]
def homeStatuses : Conformance.Family HomeStatusInput ReportOutput where
  inputs :=
    let named : ReportCase := ⟨.named, 7, some 1_000⟩
    let absent : ReportCase := ⟨.absent, 0, some 1_000⟩
    let unavailable : ReportCase := ⟨.unavailable, 0, none⟩
    [ ⟨true, named, some ⟨true, true, 1, true, named⟩⟩,
      ⟨true, absent, some ⟨true, true, 1, true, absent⟩⟩,
      ⟨true, unavailable, some ⟨true, true, 1, true, unavailable⟩⟩,
      ⟨true, named, none⟩,
      ⟨true, named, some ⟨false, true, 1, true, named⟩⟩,
      ⟨true, named, some ⟨true, true, 2, true, named⟩⟩,
      ⟨true, ⟨.named, 6, some 900⟩, some ⟨true, true, 2, true, ⟨.named, 6, some 900⟩⟩⟩,
      ⟨true, named, some ⟨true, false, 1, true, named⟩⟩,
      ⟨true, ⟨.named, 8, some 1_000⟩, some ⟨true, true, 1, true, named⟩⟩,
      ⟨true, ⟨.named, 7, some 2_000⟩, some ⟨true, true, 1, true, named⟩⟩,
      ⟨false, named, some ⟨false, true, 1, false, named⟩⟩ ]
  model := fun input =>
    ReportOutput.of
      (reportedOfHome "home" 1
        ⟨node input.namesHome, input.report.report, input.signature.map SignatureCase.signature⟩)

end Identity.HeldAudience

namespace Identity.HeldAudience

structure ClockReading where
  providerAtRead : Nat
  clockAtRead : Nat
  deriving Lean.ToJson

structure ClockReadInput where
  held : Option ClockReading
  read : Option ClockReading
  deriving Lean.ToJson

def ClockReading.clock : Option ClockReading → Option UserDelegation.HomeClock :=
  Option.map fun reading => ⟨reading.providerAtRead, reading.clockAtRead⟩

/-- A first read, a read reporting no clock, reads within the allowance either
way, and reads past it either way. -/
@[conformance_family]
def clockMoves : Conformance.Family ClockReadInput Bool where
  inputs :=
    [ ⟨none, some ⟨1_000, 900⟩⟩, ⟨some ⟨1_000, 900⟩, none⟩, ⟨none, none⟩,
      ⟨some ⟨1_000, 900⟩, some ⟨1_130, 1_000⟩⟩, ⟨some ⟨1_000, 900⟩, some ⟨1_070, 1_000⟩⟩,
      ⟨some ⟨1_000, 900⟩, some ⟨1_131, 1_000⟩⟩, ⟨some ⟨1_000, 900⟩, some ⟨1_069, 1_000⟩⟩ ]
  model := fun input => clockMoved (ClockReading.clock input.held) (ClockReading.clock input.read)

end Identity.HeldAudience
