import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.ToolTerminalEvidence

namespace Conformance.Contracts

open Conformance.ContractCases

def toolTerminalEvidenceCaseJson (row : ToolTerminalEvidenceCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString row.name ++ ","
    ++ "\"operation\":" ++ jsonString row.operation ++ ","
    ++ "\"disposition\":" ++ jsonString row.disposition ++ ","
    ++ "\"evidence_kind\":" ++ jsonString row.evidenceKind ++ ","
    ++ "\"terminal_phase\":" ++ jsonString row.terminalPhase ++ ","
    ++ "\"omission_reason\":" ++ jsonString row.omissionReason ++ ","
    ++ "\"exact_projection\":" ++ boolString row.exactProjection ++ ","
    ++ "\"evidence_closed\":" ++ boolString row.evidenceClosed ++ ","
    ++ "\"mutually_exclusive\":" ++ boolString row.mutuallyExclusive ++ ","
    ++ "\"owner_preserved\":" ++ boolString row.ownerPreserved ++ ","
    ++ "\"phase_reason_valid\":" ++ boolString row.phaseReasonValid ++ ","
    ++ "\"exact_approval_bound\":" ++ boolString row.exactApprovalBound ++ ","
    ++ "\"immutable_noop\":" ++ boolString row.immutableNoop
    ++ "}"

def toolTerminalEvidenceCasesJson : String :=
  jsonArray (toolTerminalEvidenceCases.map toolTerminalEvidenceCaseJson)

end Conformance.Contracts
