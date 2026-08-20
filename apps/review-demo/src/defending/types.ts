import type { AgentRequestRow, InferenceCallRow } from "../graph/types.ts";

export type DefenseJobRow = {
  _docID?: string;
  run_id: string;
  repository_path?: string;
  focus?: string;
  area_min?: string;
  area_max?: string;
  engagement_context?: string;
};

export type ThreatModelRow = {
  _docID?: string;
  run_id: string;
  repository_path?: string;
  focus?: string;
  area_min?: string;
  area_max?: string;
  system_context?: string;
  assets?: string;
  entry_points?: string;
  threats?: string;
  deprioritized?: string;
  open_questions?: string;
  mitigations?: string;
  provenance?: string;
};

export type DefenseAreaRow = {
  _docID?: string;
  run_id: string;
  area_id: string;
  repository_path?: string;
  focus?: string;
  threat_ids?: string;
  trust_boundary?: string;
  reachable_assets?: string;
  instructions?: string;
  expected_total?: string;
};

export type DefenseScanRow = {
  _docID?: string;
  run_id: string;
  area_id: string;
  repository_path?: string;
  expected_total?: string;
  finding_count?: string;
  coverage?: string;
  summary?: string;
};

export type DefenseCandidateRow = {
  _docID?: string;
  run_id: string;
  finding_id: string;
  area_id?: string;
  category?: string;
  claimed_severity?: string;
  confidence?: string;
  path?: string;
  line?: string;
  title?: string;
  description?: string;
  exploit_scenario?: string;
  recommendation?: string;
  evidence?: string;
  threat_ids?: string;
};

export type DefenseVerdictRow = {
  _docID?: string;
  run_id: string;
  finding_id: string;
  area_id?: string;
  verdict?: string;
  severity?: string;
  confidence?: string;
  title?: string;
  verification?: string;
  duplicate_of?: string;
  preconditions?: string;
  access_level?: string;
  owner_hint?: string;
};

export type VerificationAssignmentRow = {
  _docID?: string;
  run_id: string;
  assignment_id: string;
  finding_id: string;
  area_id?: string;
  repository_path?: string;
  status?: string;
  expected_total?: string;
};

export type VerificationCompletionRow = {
  _docID?: string;
  run_id: string;
  assignment_id: string;
  finding_id: string;
  repository_path?: string;
  status?: string;
  expected_total?: string;
};

export type DefendingFindingRow = DefenseVerdictRow & {
  category?: string;
  path?: string;
  line?: string;
  description?: string;
  exploit_scenario?: string;
  recommendation?: string;
  evidence?: string;
  threat_ids?: string;
};

export type TriageSummaryRow = {
  _docID?: string;
  run_id: string;
  candidate_count?: string;
  confirmed_count?: string;
  refuted_count?: string;
  duplicate_count?: string;
  patch_assignment_count?: string;
  summary?: string;
};

export type PatchAssignmentRow = {
  _docID?: string;
  run_id: string;
  assignment_id: string;
  finding_id?: string;
  repository_path?: string;
  status?: string;
  expected_total?: string;
};

export type PatchCandidateRow = {
  _docID?: string;
  run_id: string;
  patch_id: string;
  finding_id?: string;
  status?: string;
  repository_path?: string;
  path?: string;
  line?: string;
  category?: string;
  diff?: string;
  rationale?: string;
  variants_checked?: string;
  bypass_considered?: string;
  test_note?: string;
  expected_total?: string;
};

export type PatchReviewRow = {
  _docID?: string;
  run_id: string;
  patch_id: string;
  finding_id?: string;
  verdict?: string;
  style_score?: string;
  out_of_scope_hunks?: string;
  new_surface?: string;
  reason?: string;
  expected_total?: string;
};

export type DefenseReportRow = {
  _docID?: string;
  run_id: string;
  candidate_count?: string;
  confirmed_count?: string;
  refuted_count?: string;
  patch_count?: string;
  accepted_patch_count?: string;
  rejected_patch_count?: string;
  severity_counts?: string;
  top_risks?: string;
  summary?: string;
  human_actions?: string;
};

export type DefenseSnapshot = {
  jobs: DefenseJobRow[];
  threats: ThreatModelRow[];
  areas: DefenseAreaRow[];
  scans: DefenseScanRow[];
  candidates: DefenseCandidateRow[];
  verificationAssignments: VerificationAssignmentRow[];
  verificationCompletions: VerificationCompletionRow[];
  verdicts: DefenseVerdictRow[];
  findings: DefendingFindingRow[];
  triage: TriageSummaryRow[];
  assignments: PatchAssignmentRow[];
  patches: PatchCandidateRow[];
  reviews: PatchReviewRow[];
  reports: DefenseReportRow[];
  requests: AgentRequestRow[];
  calls: InferenceCallRow[];
};

export type DefenseNodeState =
  "expected" | "live" | "done" | "failed" | "waiting-group" | "input-required";

export type DefenseNodeKind =
  | "job"
  | "threat"
  | "plan"
  | "area"
  | "scan"
  | "triage"
  | "candidate"
  | "verification-plan"
  | "verification-assignment"
  | "verifier"
  | "verdict"
  | "assignment"
  | "patch"
  | "review"
  | "report";

export type DefenseNode = {
  id: string;
  kind: DefenseNodeKind;
  label: string;
  detail?: string;
  state: DefenseNodeState;
  runId: string;
  requestId?: string;
  sessionId?: string;
  sourceDocId?: string;
  badges: string[];
};

export type DefenseGraph = {
  runId: string | null;
  nodes: DefenseNode[];
};
