import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";
import type { TranscriptEntry } from "@/screens/transcript-groups";
import { turnTools } from "./turnChanges";

/** A turn longer than this says how long it worked beside its reply's
    time; a shorter one says so only over it. */
export const LONG_TURN_MS = 60_000;

/* past this the times are not a turn's work but a wrong clock */
const ABSURD_TURN_MS = 24 * 60 * 60_000;

const at = (iso: string | null | undefined) => {
  const ms = iso ? Date.parse(iso) : NaN;
  return Number.isNaN(ms) ? null : ms;
};

/** How long a finished turn worked: from its first tool call's start to
    its final reply. No request times reach the transcript, and a person's
    message can wait in a queue or for a runtime before its turn begins, so
    the turn's own calls mark its start. Null when that can't be told: no
    call started, the reply has no time, a call started after the reply, or
    the span is beyond any turn. */
export function turnWorked(
  tools: readonly RenderedToolCallView[],
  replied: string | null | undefined,
): number | null {
  const end = at(replied);
  if (end === null) return null;
  let start: number | null = null;
  for (const tool of tools) {
    const began = at(tool.startedAt);
    if (began === null) continue;
    if (began > end) return null;
    if (start === null || began < start) start = began;
  }
  if (start === null) return null;
  const ms = end - start;
  return ms > ABSURD_TURN_MS ? null : ms;
}

/** For each finished turn's final reply, by its entry key, how long the
    turn worked, for the turns where that can be told. */
export function turnsWorked(
  entries: readonly TranscriptEntry[],
  finalKeys: ReadonlySet<string>,
): Map<string, number> {
  const worked = new Map<string, number>();
  for (const [key, { reply, tools }] of turnTools(entries, finalKeys)) {
    const ms = turnWorked(tools, "timestamp" in reply ? reply.timestamp : null);
    if (ms !== null) worked.set(key, ms);
  }
  return worked;
}
