import type { ReviewSnapshot } from "../graph/types.ts";
import { escapeGraphqlString, postGraphql } from "./graphql.ts";

export type RuntimeHealth = "offline" | "ready" | "query-failed";

export type SessionPayload = {
  prompt?: string;
  promptTokens: number;
  completionTokens: number;
  messages: {
    sequence?: number;
    role?: string;
    content?: string;
    timestamp?: string;
  }[];
  tools: {
    tool_name?: string;
    status?: string;
    lifecycle_state?: string;
    args?: string;
    result?: string;
  }[];
  response?: {
    content?: string;
    status?: string;
    token_count?: number | null;
    error_message?: string | null;
  };
};

type JobList = { ReviewJob?: { _docID?: string; run_id: string; focus?: string }[] };

type RunData = {
  ReviewArea?: ReviewSnapshot["areas"];
  CandidateFinding?: ReviewSnapshot["candidates"];
  ScanResult?: ReviewSnapshot["scans"];
  FindingVerdict?: ReviewSnapshot["verdicts"];
  VerificationSummary?: ReviewSnapshot["summaries"];
  Finding?: ReviewSnapshot["findings"];
  TriageReport?: ReviewSnapshot["reports"];
  AgentRequest?: ReviewSnapshot["requests"];
  InferenceCall?: ReviewSnapshot["calls"];
};

export async function probeHealth(): Promise<boolean> {
  try {
    const response = await fetch("/healthz", { cache: "no-store" });
    return response.ok;
  } catch {
    return false;
  }
}

export async function loadSnapshot(): Promise<ReviewSnapshot> {
  const data = await postGraphql<JobList & RunData>(`{
    ReviewJob { _docID run_id focus repository_path base_ref head_ref lens_count lens_min lens_max pr_number }
    ReviewArea { _docID run_id area_id lens expected_total repository_path path instructions baseline }
    CandidateFinding { finding_id area_id run_id }
    ScanResult { _docID run_id area_id expected_total summary }
    FindingVerdict { finding_id run_id area_id verdict title severity evidence verification }
    VerificationSummary { _docID run_id candidate_count confirmed_count refuted_count summary }
    Finding { finding_id run_id title verdict severity }
    TriageReport { _docID run_id high_priority_count confirmed_count refuted_count summary }
    AgentRequest {
      request_id session_id behavior_id lifecycle_state
      caused_by_trigger_id caused_by_correlation caused_by_source_doc_id created_at
    }
    InferenceCall { request_id prompt_tokens completion_tokens }
  }`);
  const jobRows = data.ReviewJob ?? [];
  if (jobRows.length === 0) {
    return emptySnapshot();
  }
  return {
    jobs: jobRows,
    areas: data.ReviewArea ?? [],
    candidates: data.CandidateFinding ?? [],
    scans: data.ScanResult ?? [],
    verdicts: data.FindingVerdict ?? [],
    summaries: data.VerificationSummary ?? [],
    findings: data.Finding ?? [],
    reports: data.TriageReport ?? [],
    requests: data.AgentRequest ?? [],
    calls: data.InferenceCall ?? [],
  };
}

export async function loadSession(requestId: string, sessionId?: string): Promise<SessionPayload> {
  const request = escapeGraphqlString(requestId);
  const sessionFilter = sessionId
    ? `AgentMessage(filter: { session_id: { _eq: "${escapeGraphqlString(sessionId)}" } }, order: { sequence: ASC }) {
        sequence role content timestamp
      }`
    : `AgentMessage(filter: { request_id: { _eq: "${request}" } }, order: { sequence: ASC }) {
        sequence role content timestamp
      }`;
  const data = await postGraphql<{
    AgentRequest?: { content?: string }[];
    AgentMessage?: SessionPayload["messages"];
    AgentToolCall?: SessionPayload["tools"];
    AgentResponse?: SessionPayload["response"][];
    InferenceCall?: { prompt_tokens?: number | null; completion_tokens?: number | null }[];
  }>(`{
    AgentRequest(filter: { request_id: { _eq: "${request}" } }) {
      content
    }
    ${sessionFilter}
    AgentToolCall(filter: { request_id: { _eq: "${request}" } }) {
      tool_name status lifecycle_state args result
    }
    AgentResponse(filter: { request_id: { _eq: "${request}" } }) {
      content status token_count error_message
    }
    InferenceCall(filter: { request_id: { _eq: "${request}" } }) {
      prompt_tokens completion_tokens
    }
  }`);
  const calls = data.InferenceCall ?? [];
  const promptTokens = calls.reduce((sum, call) => sum + (call.prompt_tokens ?? 0), 0);
  const completionTokens = calls.reduce((sum, call) => sum + (call.completion_tokens ?? 0), 0);
  const firstUser = (data.AgentMessage ?? []).find((message) => message.role === "user")?.content;
  return {
    prompt: data.AgentRequest?.[0]?.content || firstUser || "",
    promptTokens,
    completionTokens,
    messages: data.AgentMessage ?? [],
    tools: data.AgentToolCall ?? [],
    response: data.AgentResponse?.[0],
  };
}

function emptySnapshot(): ReviewSnapshot {
  return {
    jobs: [],
    areas: [],
    candidates: [],
    scans: [],
    verdicts: [],
    summaries: [],
    findings: [],
    reports: [],
    requests: [],
    calls: [],
  };
}
