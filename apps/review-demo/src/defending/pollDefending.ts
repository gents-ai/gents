import { postGraphql } from "../live/graphql.ts";
import type { DefenseSnapshot } from "./types.ts";

type DefenseData = {
  DefendingCodeJob?: DefenseSnapshot["jobs"];
  DefenseThreatModel?: DefenseSnapshot["threats"];
  DefenseReviewArea?: DefenseSnapshot["areas"];
  DefenseScanResult?: DefenseSnapshot["scans"];
  DefenseCandidateFinding?: DefenseSnapshot["candidates"];
  DefenseFindingVerdict?: DefenseSnapshot["verdicts"];
  DefendingFinding?: DefenseSnapshot["findings"];
  DefenseTriageSummary?: DefenseSnapshot["triage"];
  DefensePatchAssignment?: DefenseSnapshot["assignments"];
  DefensePatchCandidate?: DefenseSnapshot["patches"];
  DefensePatchReview?: DefenseSnapshot["reviews"];
  DefenseReport?: DefenseSnapshot["reports"];
  AgentRequest?: DefenseSnapshot["requests"];
  InferenceCall?: DefenseSnapshot["calls"];
};

export async function loadDefenseSnapshot(): Promise<DefenseSnapshot> {
  const data = await postGraphql<DefenseData>(`{
    DefendingCodeJob { _docID run_id repository_path focus area_min area_max engagement_context }
    DefenseThreatModel {
      _docID run_id repository_path focus area_min area_max system_context assets entry_points
      threats deprioritized open_questions mitigations provenance
    }
    DefenseReviewArea {
      _docID run_id area_id repository_path focus threat_ids trust_boundary reachable_assets
      instructions expected_total
    }
    DefenseScanResult {
      _docID run_id area_id repository_path expected_total finding_count coverage summary
    }
    DefenseCandidateFinding {
      _docID run_id finding_id area_id category claimed_severity confidence path line title
      description exploit_scenario recommendation evidence threat_ids
    }
    DefenseFindingVerdict {
      _docID run_id finding_id area_id verdict severity confidence title verification duplicate_of
      preconditions access_level owner_hint
    }
    DefendingFinding {
      _docID run_id finding_id area_id category severity confidence path line title description
      exploit_scenario recommendation evidence verification preconditions access_level owner_hint
      threat_ids verdict
    }
    DefenseTriageSummary {
      _docID run_id candidate_count confirmed_count refuted_count duplicate_count
      summary
    }
    DefensePatchAssignment {
      _docID run_id assignment_id finding_id repository_path status expected_total
    }
    DefensePatchCandidate {
      _docID run_id patch_id finding_id status repository_path path line category diff rationale
      variants_checked bypass_considered test_note expected_total
    }
    DefensePatchReview {
      _docID run_id patch_id finding_id verdict style_score out_of_scope_hunks new_surface reason
      expected_total
    }
    DefenseReport {
      _docID run_id candidate_count confirmed_count refuted_count patch_count accepted_patch_count
      rejected_patch_count severity_counts top_risks summary human_actions
    }
    AgentRequest {
      request_id session_id behavior_id status lifecycle_state caused_by_trigger_id
      caused_by_correlation caused_by_source_doc_id caused_by_parent_request_id
      caused_by_parent_tool_call_id subagent_depth content created_at
    }
    InferenceCall { request_id prompt_tokens completion_tokens }
  }`);
  let verificationAssignments: DefenseSnapshot["verificationAssignments"] = [];
  let verificationCompletions: DefenseSnapshot["verificationCompletions"] = [];
  let clusters: DefenseSnapshot["clusters"] = [];
  let contractReviews: DefenseSnapshot["contractReviews"] = [];
  let validations: DefenseSnapshot["validations"] = [];
  let securityReviews: DefenseSnapshot["securityReviews"] = [];
  let contractPipelineAvailable = false;
  try {
    const optional = await postGraphql<{
      DefenseVerificationAssignment?: DefenseSnapshot["verificationAssignments"];
      DefenseVerificationCompletion?: DefenseSnapshot["verificationCompletions"];
    }>(`{
      DefenseVerificationAssignment {
        _docID run_id assignment_id finding_id area_id repository_path status expected_total
      }
      DefenseVerificationCompletion {
        _docID run_id assignment_id finding_id repository_path status expected_total
      }
    }`);
    verificationAssignments = optional.DefenseVerificationAssignment ?? [];
    verificationCompletions = optional.DefenseVerificationCompletion ?? [];
  } catch {
    // Older live runs predate the assignment schema. Keep their visualizer usable.
  }
  try {
    const optional = await postGraphql<{
      DefenseRootCauseCluster?: DefenseSnapshot["clusters"];
      DefenseContractReview?: DefenseSnapshot["contractReviews"];
      DefensePatchValidation?: DefenseSnapshot["validations"];
      DefensePatchSecurityReview?: DefenseSnapshot["securityReviews"];
    }>(`{
      DefenseRootCauseCluster {
        _docID run_id cluster_id repository_path base_revision status primary_finding_id
        member_finding_ids canonical_title canonical_root_cause severity security_boundary
        expected_total
      }
      DefenseContractReview {
        _docID run_id review_id cluster_id status disposition spec_impact
        required_foundation_flow recommended_fix_boundary evidence expected_total
      }
      DefensePatchValidation {
        _docID run_id validation_id patch_id cluster_id finding_id status applies_cleanly
        format_status compile_status test_status proof_status commands evidence expected_total
      }
      DefensePatchSecurityReview {
        _docID run_id security_review_id patch_id cluster_id finding_id verdict
        original_path_closed sibling_variants_checked bypass_found contract_alignment evidence
        expected_total
      }
    }`);
    clusters = optional.DefenseRootCauseCluster ?? [];
    contractReviews = optional.DefenseContractReview ?? [];
    validations = optional.DefensePatchValidation ?? [];
    securityReviews = optional.DefensePatchSecurityReview ?? [];
    contractPipelineAvailable = true;
  } catch {
    // Older live runs predate the contract-aware patch pipeline.
  }
  return {
    contractPipelineAvailable,
    jobs: data.DefendingCodeJob ?? [],
    threats: data.DefenseThreatModel ?? [],
    areas: data.DefenseReviewArea ?? [],
    scans: data.DefenseScanResult ?? [],
    candidates: data.DefenseCandidateFinding ?? [],
    verificationAssignments,
    verificationCompletions,
    verdicts: data.DefenseFindingVerdict ?? [],
    findings: data.DefendingFinding ?? [],
    triage: data.DefenseTriageSummary ?? [],
    clusters,
    contractReviews,
    assignments: data.DefensePatchAssignment ?? [],
    patches: data.DefensePatchCandidate ?? [],
    validations,
    reviews: data.DefensePatchReview ?? [],
    securityReviews,
    reports: data.DefenseReport ?? [],
    requests: data.AgentRequest ?? [],
    calls: data.InferenceCall ?? [],
  };
}

export function emptyDefenseSnapshot(): DefenseSnapshot {
  return {
    contractPipelineAvailable: false,
    jobs: [],
    threats: [],
    areas: [],
    scans: [],
    candidates: [],
    verificationAssignments: [],
    verificationCompletions: [],
    verdicts: [],
    findings: [],
    triage: [],
    clusters: [],
    contractReviews: [],
    assignments: [],
    patches: [],
    validations: [],
    reviews: [],
    securityReviews: [],
    reports: [],
    requests: [],
    calls: [],
  };
}
