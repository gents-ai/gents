import type {
  RenderedTimelineItem,
  RenderedToolCallView,
} from "@source-inc/gents-desktop-client";

import { groupTranscript, type TranscriptEntry } from "@/screens/transcript-groups";
import { assistantMessage, userMessage } from "./timeline-fixture";

/* a deterministic stream of words, so every run searches the same text */
function words(seed: number, length: number): string {
  const vocabulary =
    "the build failed because a test timed out while the compiler checked the module graph for cycles and the linker wrote symbols into the cache directory under target".split(
      " ",
    );
  let out = "";
  let n = seed;
  while (out.length < length) {
    n = (n * 1103515245 + 12345) & 0x7fffffff;
    out += `${vocabulary[n % vocabulary.length]}${n % 17 === 0 ? `_${n % 997}` : ""} `;
  }
  return out;
}

export const command = (key: string, stdout: string): RenderedToolCallView =>
  ({
    itemKey: key,
    toolName: "command",
    statusKind: "success",
    presentation: {
      kind: "command",
      command: `cargo test -p gents ${key}`,
      exitCode: 0,
      timedOut: false,
      failed: false,
      durationMs: 1200,
      cwd: "~/work",
      executionMode: "sandboxed",
      networkMode: "disabled",
      stdout,
      stderr: "",
      fallbackOutput: null,
    },
  }) as unknown as RenderedToolCallView;

/* a working session: each turn a message, a group of steps (some with
   tens of KB of output) and a long reply, until `rows` items */
export function workingSession(rows: number): TranscriptEntry[] {
  const items: RenderedTimelineItem[] = [];
  for (let turn = 0; items.length < rows; turn += 1) {
    items.push(userMessage({ itemKey: `u${turn}`, content: words(turn, 240) }));
    const tools = Array.from({ length: 1 + (turn % 3) }, (_, i) =>
      command(
        `t${turn}-${i}`,
        words(turn * 7 + i, i === 0 && turn % 2 === 0 ? 30_000 : 1_500),
      ),
    );
    items.push({
      kind: "toolGroup",
      itemKey: `g${turn}`,
      messageSequence: null,
      tools,
    });
    items.push(
      assistantMessage({
        itemKey: `a${turn}`,
        content: words(turn * 13, 3_000),
        reasoning: turn % 4 === 0 ? words(turn * 3, 800) : null,
      }),
    );
  }
  return groupTranscript(items.slice(0, rows));
}

/* the harness's large session: short messages and replies */
export function harnessSession(rows: number): TranscriptEntry[] {
  const filler =
    "bounded durable transcript fixture with markdown `code`, stable prose, and comparable byte shape";
  return groupTranscript(
    Array.from({ length: rows }, (_, index) =>
      index % 2 === 0
        ? userMessage({
            itemKey: `u${index}`,
            content: `User fixture row ${index}: ${filler}`,
          })
        : assistantMessage({
            itemKey: `a${index}`,
            content: `Assistant fixture row ${index}: ${filler}`,
            reasoning: index % 10 === 1 ? `Reasoning fixture ${index}` : null,
          }),
    ),
  );
}
