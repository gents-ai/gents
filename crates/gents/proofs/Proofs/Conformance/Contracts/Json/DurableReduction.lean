import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.Contracts.Json.PromptAssembly
import Proofs.Conformance.ContractCases.DurableReduction

namespace Conformance.Contracts

open Conformance.ContractCases

def durableReductionCaseJson (witness : DurableReductionCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString witness.name ++ ","
    ++ "\"request_doc_id\":" ++ toString witness.requestDocId ++ ","
    ++ "\"turn_index\":" ++ toString witness.turnIndex ++ ","
    ++ "\"ordinal\":" ++ toString witness.ordinal ++ ","
    ++ "\"checkpoint\":" ++ toString witness.checkpoint ++ ","
    ++ "\"claim_commit\":" ++ toString witness.claimCommit ++ ","
    ++ "\"prior_checkpoint\":" ++ jsonOptionalNat witness.priorCheckpoint ++ ","
    ++ "\"prior_claim_commit\":" ++ jsonOptionalNat witness.priorClaimCommit ++ ","
    ++ "\"pair_closed\":" ++ boolString witness.pairClosed ++ ","
    ++ "\"inference_cites\":" ++ boolString witness.inferenceCites ++ ","
    ++ "\"inference_supported\":" ++ boolString witness.inferenceSupported ++ ","
    ++ "\"title_cites\":" ++ boolString witness.titleCites ++ ","
    ++ "\"outcome\":" ++ jsonString witness.outcome ++ ","
    ++ "\"durable_after\":" ++ boolString witness.durableAfter ++ ","
    ++ "\"send_permitted\":" ++ boolString witness.sendPermitted ++ ","
    ++ "\"consumed\":" ++ boolString witness.consumed
    ++ "}"

def durableReductionCasesJson : String :=
  jsonArray (durableReductionCases.map durableReductionCaseJson)

private def reductionKeyJson (key : Compaction.DurableReduction.ReductionKey) : String :=
  "{\"agent_did\":" ++ toString key.agentDid ++
    ",\"session_id\":" ++ toString key.sessionId ++
    ",\"request_doc_id\":" ++ toString key.requestDocId ++
    ",\"turn_index\":" ++ toString key.turnIndex ++
    ",\"ordinal\":" ++ toString key.ordinal ++ "}"

private def reductionProjectionJson
    (projection : Compaction.DurableReduction.Projection) : String :=
  "{\"value\":" ++ toString projection.value ++
    ",\"tagged_rows\":" ++
      (match projection.taggedRows with
       | none => "null"
       | some rows => jsonArray (rows.map claudeTaggedReplayRowJson)) ++
    ",\"retired\":" ++ jsonArray (projection.retired.map claudeReplayTagJson) ++ "}"

private def reductionFactJson (fact : Compaction.DurableReduction.Fact) : String :=
  "{\"claim_commit\":" ++ toString fact.claimCommit ++
    ",\"source_boundary\":" ++ toString fact.sourceBoundary.value ++
    ",\"source_projection\":" ++ reductionProjectionJson fact.sourceProjection ++
    ",\"checkpoint\":" ++ reductionProjectionJson fact.checkpoint ++
    ",\"producer_call\":" ++ jsonOptionalNat fact.producerCall ++
    ",\"parent\":" ++
      (match fact.parent with
       | none => "null"
       | some key => reductionKeyJson key) ++
    ",\"pair_closed\":" ++ boolString fact.pairClosed ++ "}"

private def sourceObservationJson
    (observation : Compaction.DurableReduction.SourceObservation) : String :=
  "{\"tag\":" ++ claudeReplayTagJson observation.tag ++
    ",\"agent_did\":" ++ toString observation.agentDid ++
    ",\"session_id\":" ++ toString observation.sessionId ++
    ",\"source_boundary\":" ++ toString observation.sourceBoundary.value ++ "}"

private def captureCitationJson
    (citation : Compaction.DurableReduction.CaptureCitation) : String :=
  "{\"kind\":" ++ jsonString (match citation.kind with
       | .inference => "inference"
       | .title => "title"
       | .compaction => "compaction") ++
    ",\"supported\":" ++ boolString citation.supported ++
    ",\"reduction_keys\":" ++ jsonArray (citation.reductionKeys.map reductionKeyJson) ++ "}"

def durableFullInputRewriteCaseJson (witness : DurableFullInputRewriteCase) : String :=
  "{\"name\":" ++ jsonString witness.name ++
    ",\"key\":" ++ reductionKeyJson witness.key ++
    ",\"lineage\":" ++ jsonArray (witness.lineage.map reductionKeyJson) ++
    ",\"prior\":" ++
      (match witness.prior with
       | none => "null"
       | some (key, fact) => "{\"key\":" ++ reductionKeyJson key ++
           ",\"fact\":" ++ reductionFactJson fact ++ "}") ++
    ",\"observations\":" ++
      jsonArray (witness.observations.map sourceObservationJson) ++
    ",\"fact\":" ++ reductionFactJson witness.fact ++
    ",\"captures\":" ++ jsonArray (witness.captures.map captureCitationJson) ++
    ",\"prior_consumed\":" ++ boolString witness.priorConsumed ++
    ",\"outcome\":" ++ jsonString witness.outcome ++
    ",\"retired\":" ++ jsonArray (witness.retired.map claudeReplayTagJson) ++ "}"

def durableFullInputRewriteCasesJson : String :=
  jsonArray (durableFullInputRewriteCases.map durableFullInputRewriteCaseJson)

private def durableSessionRewriteActionJson
    (action : DurableSessionRewriteAction) : String :=
  "{\"key\":" ++ reductionKeyJson action.key ++
    ",\"lineage\":" ++ jsonArray (action.lineage.map reductionKeyJson) ++
    ",\"observations\":" ++
      jsonArray (action.observations.map sourceObservationJson) ++
    ",\"fact\":" ++ reductionFactJson action.fact ++
    ",\"cursor\":" ++ toString action.cursor ++ "}"

private def sessionCursorEntryJson
    (entry : Compaction.DurableReduction.SessionCursorEntry) : String :=
  "{\"key\":" ++ reductionKeyJson entry.key ++
    ",\"cursor\":" ++ toString entry.cursor ++ "}"

def durableSessionRewriteCaseJson (witness : DurableSessionRewriteCase) : String :=
  "{\"name\":" ++ jsonString witness.name ++
    ",\"actions\":" ++ jsonArray (witness.actions.map durableSessionRewriteActionJson) ++
    ",\"outcomes\":" ++ jsonArray (witness.outcomes.map jsonString) ++
    ",\"entries\":" ++ jsonArray (witness.entries.map sessionCursorEntryJson) ++
    ",\"stored_keys\":" ++ jsonArray (witness.storedKeys.map reductionKeyJson) ++ "}"

def durableSessionRewriteCasesJson : String :=
  jsonArray (durableSessionRewriteCases.map durableSessionRewriteCaseJson)

end Conformance.Contracts
