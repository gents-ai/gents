import Proofs.Store.TransactionReplay
import Proofs.Conformance.Family

namespace TransactionReplay

deriving instance Lean.ToJson for Mode, Stage, Failure, Disposition

attribute [conformance_function] disposition rerunHonoured Mode.maxAttempts

end TransactionReplay

namespace Conformance.TransactionReplay

open Lean _root_.TransactionReplay

inductive Entry where
  | write | signedRebuild | idempotent | observing
  deriving DecidableEq, ToJson

/-- What one attempt meets on the HTTP transport. External premise: DefraDB's
commit handler answers a store conflict with `409 Conflict`; only a body whose
`outcome` is `transaction_conflict_aborted` confirms that the commit did not
land, and a `500` says nothing about it. -/
inductive Fault where
  | commitConflictText
  | commitConflictUnreceipted
  | commitConflictAborted
  | commitServerError
  | callbackConflict
  | callbackStorageFault
  | callbackRejection
  deriving DecidableEq, ToJson

structure Scenario where
  entry : Entry
  heldSigner : Bool
  signs : Bool
  faults : List Fault
  deriving ToJson

inductive Outcome where
  | committed | conflictObserved | failed
  deriving DecidableEq, ToJson

structure Observed where
  outcome : Outcome
  begun : Nat
  callbacks : Nat
  discarded : List Nat
  commits : List Nat
  unconfirmed : Option Bool
  conflict : Option Bool
  deriving ToJson

/-- Rebuilding a signed write needs the transport to hold the client's signing
capability; without it the entry is an ordinary conflict retry. -/
def Entry.mode (heldSigner : Bool) : Entry → Mode
  | .write => .conflictRetry
  | .signedRebuild => if heldSigner then .signedConflictRebuild else .conflictRetry
  | .idempotent => .idempotent
  | .observing => .observeConflict

/-- Only the entries that report finality say whether a failed commit may
have landed. -/
def Entry.reportsFinality : Entry → Bool
  | .write | .signedRebuild => true
  | .idempotent | .observing => false

/-- A failed callback is discarded, and the discard is the store's word that
its writes did not land. -/
def Fault.attempt : Fault → Attempt
  | .commitConflictText | .commitConflictUnreceipted => .fails .commit .conflict false
  | .commitConflictAborted => .fails .commit .conflict true
  | .commitServerError => .fails .commit .storage false
  | .callbackConflict => .fails .callback .conflict true
  | .callbackStorageFault => .fails .callback .storage true
  | .callbackRejection => .fails .callback .rejection true

def Fault.inCallback : Fault → Bool
  | .callbackConflict | .callbackStorageFault | .callbackRejection => true
  | _ => false

/-- External premise: an HTTP commit consumes its transaction whether or not
it lands, so only an attempt that failed in its callback is discarded. -/
def observe (s : Scenario) : Observed :=
  let run := run (s.entry.mode s.heldSigner) s.signs (s.faults.map Fault.attempt)
  let failedInCallback := fun index => ((s.faults[index]?).map Fault.inCallback).getD false
  { outcome := match run.result with
      | .committed => .committed
      | .conflictObserved => .conflictObserved
      | .failed => .failed
    begun := run.begun
    callbacks := run.callbacks
    discarded := (List.range run.callbacks).filter failedInCallback
    commits := (List.range run.callbacks).filter (!failedInCallback ·)
    unconfirmed :=
      if run.result == .failed && s.entry.reportsFinality then some run.unconfirmed else none
    conflict := run.failure.map (· == .conflict) }

def faults : List Fault :=
  [ .commitConflictText, .commitConflictUnreceipted, .commitConflictAborted, .commitServerError
  , .callbackConflict, .callbackStorageFault, .callbackRejection ]

def entries : List (Entry × Bool) :=
  [(.write, false), (.signedRebuild, true), (.signedRebuild, false), (.idempotent, false),
   (.observing, false)]

@[conformance_family]
def http : Family Scenario Observed where
  inputs :=
    (entries.flatMap fun (entry, heldSigner) =>
      [false, true].flatMap fun signs =>
        ([] :: faults.map ([·])).map fun faults => { entry, heldSigner, signs, faults })
    ++ [ { entry := .write, heldSigner := false, signs := false
           faults := List.replicate 3 .commitConflictText }
       , { entry := .write, heldSigner := false, signs := false
           faults := List.replicate 4 .commitConflictText }
       , { entry := .signedRebuild, heldSigner := true, signs := true
           faults := [.commitConflictAborted, .callbackConflict, .commitConflictUnreceipted] }
       , { entry := .idempotent, heldSigner := false, signs := false
           faults := [.commitServerError, .callbackStorageFault, .callbackRejection] }
       , { entry := .write, heldSigner := false, signs := false
           faults := [.commitConflictText, .callbackRejection] } ]
  model := observe

end Conformance.TransactionReplay
