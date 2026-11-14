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

/-- Native segment payloads are UTF-8 protocol strings, so only cases whose
flushed bytes decode as UTF-8 can be written as native segments. -/
def stringPayloads (witness : StreamingResponse.ReasoningAudit.SignatureCase) : Bool :=
  witness.records.all fun record =>
    (record.flush.map fun flush =>
      (String.fromUTF8? (ByteArray.mk flush.payload.toArray)).isSome).getD true

def signatureAccepted (i : SignatureInput) : Bool :=
  ((signatureNamed i.name).map fun witness =>
    (CanonicalOutput.validateReasoningSignature witness.records [] witness.payload
      witness.signature).isOk).getD false

@[conformance_family]
def signature : Family SignatureInput Bool where
  inputs := (StreamingResponse.ReasoningAudit.signatureCases.filter stringPayloads).map signatureInput
  model := signatureAccepted

/-- model-production-gap: native OutputSegment payload is a UTF-8 String, so a
flush whose modeled bytes are not UTF-8 cannot be written or translated; the
model rejects such a signature run as malformed. -/
@[conformance_family]
def signatureUnreached : Family SignatureInput Bool where
  inputs := (StreamingResponse.ReasoningAudit.signatureCases.filter (!stringPayloads ·)).map
    signatureInput
  model := signatureAccepted

example : (StreamingResponse.ReasoningAudit.signatureCases.filter (!stringPayloads ·)).map
    (·.name) = ["invalid_signature_bytes"] := by
  native_decide

example : (StreamingResponse.ReasoningAudit.signatureCases.filter (!stringPayloads ·)).all
    (!·.accepted) := by
  native_decide

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
