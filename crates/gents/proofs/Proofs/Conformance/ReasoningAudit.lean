import Proofs.Conformance.CanonicalOutputWire
import Proofs.Session.StreamingResponse.ReasoningAudit

namespace Conformance.ReasoningAudit

open Lean

structure SignatureInput where
  name : String
  records : Json
  payload : Json
  signature : Option String
  deriving ToJson

def signatureInput (witness : StreamingResponse.ReasoningAudit.SignatureCase) : SignatureInput :=
  { name := witness.name, records := CanonicalOutputWire.segments witness.records
    payload := CanonicalOutputWire.payloadSpec witness.payload, signature := witness.signature }

def signatureNamed (name : String) : Option StreamingResponse.ReasoningAudit.SignatureCase :=
  StreamingResponse.ReasoningAudit.signatureCases.find? (·.name == name)

theorem signature_case_names_are_unique :
    (StreamingResponse.ReasoningAudit.signatureCases.map (·.name)).eraseDups.length =
      StreamingResponse.ReasoningAudit.signatureCases.length := by
  native_decide

def signatureAccepted (i : SignatureInput) : Bool :=
  ((signatureNamed i.name).map fun witness =>
    (CanonicalOutput.validateReasoningSignature witness.records [] witness.payload
      witness.signature).isOk).getD false

@[conformance_family]
def signature : Family SignatureInput Bool where
  inputs := StreamingResponse.ReasoningAudit.signatureCases.map signatureInput
  model := signatureAccepted

structure PrefixInput where
  name : String
  input : Json
  deriving ToJson

def auditNamed (name : String) : Option StreamingResponse.ReasoningAudit.AuditCase :=
  StreamingResponse.ReasoningAudit.auditCases.find? (·.name == name)

theorem audit_case_names_are_unique :
    (StreamingResponse.ReasoningAudit.auditCases.map (·.name)).eraseDups.length =
      StreamingResponse.ReasoningAudit.auditCases.length := by
  native_decide

@[conformance_family]
def «prefix» : Family PrefixInput Json where
  inputs := StreamingResponse.ReasoningAudit.auditCases.map fun witness =>
    { name := witness.name, input := CanonicalOutputWire.observation witness.input }
  model := fun i => ((auditNamed i.name).map fun witness =>
    CanonicalOutputWire.auditResult
      (StreamingResponse.reconstructAuditPrefix witness.input none)).getD Json.null

structure RecoveryEntryWire where
  stream : Nat
  blockIndex : Nat
  kind : String
  signature : Option String
  deriving ToJson

structure RecoveryOutput where
  valid : Bool
  closeStreamBytes : List Nat
  retained : List RecoveryEntryWire
  audit : Json
  deriving ToJson

def recoveryNamed (name : String) : Option StreamingResponse.ReasoningAudit.RecoveryCase :=
  StreamingResponse.ReasoningAudit.recoveryCases.find? (·.name == name)

theorem recovery_case_names_are_unique :
    (StreamingResponse.ReasoningAudit.recoveryCases.map (·.name)).eraseDups.length =
      StreamingResponse.ReasoningAudit.recoveryCases.length := by
  native_decide

def recoveryResult (witness : StreamingResponse.ReasoningAudit.RecoveryCase) : RecoveryOutput :=
  let obs := witness.input
  let audit := CanonicalOutputWire.auditResult (StreamingResponse.reconstructAuditPrefix obs none)
  match StreamingResponse.ReasoningAudit.planRecovery obs.records obs.target.coordinate
      obs.target.writer with
  | .error _ => { valid := false, closeStreamBytes := [], retained := [], audit }
  | .ok plan =>
      { valid := true
        closeStreamBytes := plan.closeStreamBytes
        retained := plan.retained.map fun entry =>
          { stream := entry.stream, blockIndex := entry.blockIndex,
            kind := entry.kind, signature := entry.signature }
        audit }

@[conformance_family]
def recovery : Family PrefixInput RecoveryOutput where
  inputs := StreamingResponse.ReasoningAudit.recoveryCases.map fun witness =>
    { name := witness.name, input := CanonicalOutputWire.observation witness.input }
  model := fun i => ((recoveryNamed i.name).map recoveryResult).getD
    { valid := false, closeStreamBytes := [], retained := [], audit := Json.null }

end Conformance.ReasoningAudit
