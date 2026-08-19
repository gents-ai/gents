import type { AgentRequestRow, InferenceCallRow } from "./types.ts";

export type TokenTotals = {
  prompt: number;
  completion: number;
  total: number;
};

export function tokenTotalsForRun(
  calls: InferenceCallRow[],
  requests: AgentRequestRow[],
  runId: string | null,
): TokenTotals {
  if (!runId) {
    return { prompt: 0, completion: 0, total: 0 };
  }
  const requestIds = new Set(
    requests
      .filter((request) => request.caused_by_correlation === runId)
      .map((request) => request.request_id),
  );
  let prompt = 0;
  let completion = 0;
  for (const call of calls) {
    if (!requestIds.has(call.request_id)) {
      continue;
    }
    prompt += call.prompt_tokens ?? 0;
    completion += call.completion_tokens ?? 0;
  }
  return { prompt, completion, total: prompt + completion };
}

export function formatTokenTotals(totals: TokenTotals): string {
  if (totals.total <= 0) {
    return "0 tokens";
  }
  return `${totals.total.toLocaleString()} tokens · ${totals.prompt.toLocaleString()} in · ${totals.completion.toLocaleString()} out`;
}
