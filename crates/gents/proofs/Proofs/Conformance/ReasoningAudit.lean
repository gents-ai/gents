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

end Conformance.ReasoningAudit
