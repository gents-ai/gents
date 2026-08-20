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
      patch_assignment_count summary
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
  return {
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
    assignments: data.DefensePatchAssignment ?? [],
    patches: data.DefensePatchCandidate ?? [],
    reviews: data.DefensePatchReview ?? [],
    reports: data.DefenseReport ?? [],
    requests: data.AgentRequest ?? [],
    calls: data.InferenceCall ?? [],
  };
}

export function emptyDefenseSnapshot(): DefenseSnapshot {
  return {
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
    assignments: [],
    patches: [],
    reviews: [],
    reports: [],
    requests: [],
    calls: [],
  };
}
