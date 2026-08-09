import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.RunTimelineManifest

namespace Conformance.Contracts

open Conformance.ContractCases

def runTimelineManifestCaseJson (row : RunTimelineManifestCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString row.name ++ ","
    ++ "\"disposition\":" ++ jsonString row.disposition ++ ","
    ++ "\"selector\":" ++ jsonString row.selector ++ ","
    ++ "\"visible_logical_roots\":" ++ toString row.visibleLogicalRoots ++ ","
    ++ "\"root_doc_id\":" ++ jsonOptionalNat row.rootDocId ++ ","
    ++ "\"root_cid\":" ++ jsonOptionalNat row.rootCid ++ ","
    ++ "\"expected_slots\":" ++ toString row.expectedSlots ++ ","
    ++ "\"included_slots\":" ++ toString row.includedSlots ++ ","
    ++ "\"omitted_slots\":" ++ toString row.omittedSlots ++ ","
    ++ "\"ordered_source_classes\":" ++ jsonStringArray row.orderedSourceClasses ++ ","
    ++ "\"ordered_collections\":" ++ jsonArray (row.orderedCollections.map toString) ++ ","
    ++ "\"ordered_collection_version_ids\":"
      ++ jsonArray (row.orderedCollectionVersionIds.map toString) ++ ","
    ++ "\"ordered_doc_ids\":" ++ jsonArray (row.orderedDocIds.map toString) ++ ","
    ++ "\"ordered_cids\":" ++ jsonArray (row.orderedCids.map toString) ++ ","
    ++ "\"exact_membership\":" ++ boolString row.exactMembership ++ ","
    ++ "\"complete_coverage\":" ++ boolString row.completeCoverage ++ ","
    ++ "\"canonical_order\":" ++ boolString row.canonicalOrder ++ ","
    ++ "\"manifest_version\":" ++ jsonOptionalNat row.manifestVersion ++ ","
    ++ "\"manifest_status\":" ++ jsonOptionalString row.manifestStatus ++ ","
    ++ "\"coverage_gap_count\":" ++ toString row.coverageGapCount ++ ","
    ++ "\"ordered_coverage_gap_kinds\":"
      ++ jsonStringArray row.orderedCoverageGapKinds ++ ","
    ++ "\"canonical_gaps\":" ++ boolString row.canonicalGaps
    ++ "}"

def runTimelineManifestCasesJson : String :=
  jsonArray (runTimelineManifestCases.map runTimelineManifestCaseJson)

end Conformance.Contracts
