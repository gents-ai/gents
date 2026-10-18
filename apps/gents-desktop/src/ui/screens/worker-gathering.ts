import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";

export type GatheredWorker = { key: string; tools: RenderedToolCallView[] };

const workerKey = (tool: RenderedToolCallView) => {
  const p = tool.presentation;
  if (p.kind !== "agent") return null;
  return p.sessionId ?? p.name ?? null;
};

export function gatherWorkers(
  tools: readonly RenderedToolCallView[],
): ReadonlyMap<string, GatheredWorker> {
  const byWorker = new Map<string, RenderedToolCallView[]>();
  for (const tool of tools) {
    const key = workerKey(tool);
    if (key) byWorker.set(key, [...(byWorker.get(key) ?? []), tool]);
  }
  const byTool = new Map<string, GatheredWorker>();
  for (const [key, its] of byWorker) {
    if (its.length < 2) continue;
    const worker = { key, tools: its };
    for (const tool of its) byTool.set(tool.itemKey, worker);
  }
  return byTool;
}

const VERB: Record<string, string> = {
  start: "started",
  message: "messaged",
  interrupt: "interrupted",
};

export function workerStory(tools: readonly RenderedToolCallView[]): string {
  const counts = new Map<string, number>();
  for (const tool of tools) {
    const p = tool.presentation;
    if (p.kind !== "agent") continue;
    const verb = VERB[p.action] ?? p.action;
    counts.set(verb, (counts.get(verb) ?? 0) + 1);
  }
  return [...counts].map(([verb, n]) => (n > 1 ? `${verb} ×${n}` : verb)).join(" · ");
}
