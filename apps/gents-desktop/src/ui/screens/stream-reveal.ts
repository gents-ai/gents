/* Streaming text, revealed at a steady pace.

   The runtime does not stream tokens to the desktop. It writes what the
   model has said so far every `stream_batch_ms` (1,000 by default, and on
   purpose: each write is a signed DefraDB version that replicates), and the
   desktop picks that up on its own clock — an update event or a 1.5s poll.
   So the text arrives as a burst every second or so, of whatever size the
   two clocks happened to leave between them: a word, or a paragraph.
   Shown as it lands, that reads as a stutter.

   Nothing here changes what arrives. The screen shows a prefix of it and
   moves that prefix forward at a steady rate, sized so a burst drains in
   about the time the next one takes to come: the text trails the stream by
   up to one burst and never stops and starts. The rate is measured, not
   assumed — a stream that arrives word by word is revealed word by word.

   Two seams make the same text disappear and come back, and both are
   closed here rather than in the runtime:
     · the live tail and the message it becomes are two items with two
       keys. Drawn under them, React removes the tail's rows and inserts
       the message's in one commit, and the browser lays the page out
       between the two: a reader inside the reply is clamped to where the
       page ended without it, and the text would print again from the
       start. `drawKeys` draws the message under its tail's key instead, so
       the same rows update in place and the reveal carries on;
     · the live text can be dropped a projection before the message that
       replaces it arrives — `holdLive` keeps it on screen through the gap. */
import type { RenderedTimelineItem } from "@source-inc/gents-desktop-client";

/* slowest a backlog is revealed, so a lone trailing word does not crawl */
const MIN_CPS = 45;
/* the measured time between bursts is clamped to this range */
const MIN_GAP_MS = 40;
const MAX_GAP_MS = 2_000;
/* a burst drains over a little more than the gap, so the next one arrives
   while the last is still being revealed and the text never stalls */
const DRAIN_OVER_GAP = 1.5;
/* never trail the stream by more than this: a large burst after a long
   silence is shown in part at once rather than typed out for seconds */
const MAX_BACKLOG = 1_200;

export type RevealState = {
  shown: number;
  /* fractional characters owed from earlier frames */
  carry: number;
  /* characters per second, fixed when a burst arrives */
  cps: number;
  targetLen: number;
  lastGrowthAt: number | null;
  gapMs: number;
};

export function initialReveal(startFrom = 0, targetLen = startFrom): RevealState {
  const shown = Math.min(startFrom, targetLen);
  return {
    shown,
    carry: 0,
    /* a tail first drawn mid-stream mounts with its text already owed:
       pace it like a burst that just arrived, not at the crawl reserved
       for a lone trailing word */
    cps: Math.max(MIN_CPS, ((targetLen - shown) / (1_000 * DRAIN_OVER_GAP)) * 1_000),
    targetLen,
    lastGrowthAt: null,
    gapMs: 1_000,
  };
}

/* one frame: note any growth, then move the shown prefix forward */
export function stepReveal(
  state: RevealState,
  targetLen: number,
  now: number,
  dtMs: number,
): RevealState {
  let { shown, carry, cps, lastGrowthAt, gapMs } = state;
  if (targetLen < shown) {
    /* the text was replaced rather than extended: show what is there */
    shown = targetLen;
    carry = 0;
  }
  if (targetLen > state.targetLen) {
    if (lastGrowthAt !== null) {
      const gap = Math.min(MAX_GAP_MS, Math.max(MIN_GAP_MS, now - lastGrowthAt));
      /* smoothed, so one late burst does not halve the pace */
      gapMs = gapMs * 0.6 + gap * 0.4;
    }
    lastGrowthAt = now;
    const backlog = targetLen - shown;
    cps = Math.max(MIN_CPS, (backlog / (gapMs * DRAIN_OVER_GAP)) * 1_000);
  }
  if (targetLen - shown > MAX_BACKLOG) shown = targetLen - MAX_BACKLOG;
  if (shown < targetLen) {
    carry += (cps * Math.max(0, dtMs)) / 1_000;
    const whole = Math.floor(carry);
    carry -= whole;
    shown = Math.min(targetLen, shown + whole);
  } else carry = 0;
  return { shown, carry, cps, targetLen, lastGrowthAt, gapMs };
}

/* The prefix to draw. It ends on a word boundary while more is coming, so
   a half-typed `**bo` never renders as literal asterisks that then flip to
   bold, and never inside a surrogate pair. */
export function revealedText(text: string, shown: number): string {
  if (shown >= text.length) return text;
  let end = shown;
  const space = text.lastIndexOf(" ", end);
  const newline = text.lastIndexOf("\n", end);
  const boundary = Math.max(space, newline);
  if (boundary > 0) end = boundary;
  const code = text.charCodeAt(end - 1);
  if (code >= 0xd800 && code <= 0xdbff) end -= 1;
  return text.slice(0, Math.max(0, end));
}

/* A code block that has opened but not closed renders everything after it
   as code, and then snaps back when the fence arrives. Close it for the
   moment it is on screen. */
export function closeOpenFence(text: string): string {
  const fences = text
    .split("\n")
    .filter((line) => line.trimStart().startsWith("```")).length;
  return fences % 2 === 1 ? `${text}\n\`\`\`` : text;
}

/* The live text as it last stood, while nothing has replaced it yet. It
   belongs to one session, and remembers which messages were already there,
   so only a message that arrives afterwards can be the one replacing it. */
export type HeldLive = {
  sessionId: string | null;
  itemKey: string;
  content: string;
  reasoning: string | null;
  earlier: string[];
};

/* Keep the live text on screen until the message that replaces it arrives.
   If the tail item is still there with its text cleared, the text goes back
   into it — same key, so the component revealing it carries on untouched;
   if the tail is gone, it is put back where it was. `held` is the caller's
   record of the last live text; the return says whether it still applies. */
export function holdLive(
  items: RenderedTimelineItem[],
  held: HeldLive | null,
  sessionId: string | null = null,
): {
  items: RenderedTimelineItem[];
  held: HeldLive | null;
  /** the message that ended the hold, when one did */
  replacedBy?: string;
} {
  /* text held for one session is never shown in another */
  if (held && held.sessionId !== sessionId) held = null;
  const live = items.find((i) => i.kind === "liveAssistant");
  if (live && live.kind === "liveAssistant" && live.content) {
    return {
      items,
      held: {
        sessionId,
        itemKey: live.itemKey,
        content: live.content,
        reasoning: live.reasoning,
        earlier:
          held?.itemKey === live.itemKey
            ? held.earlier
            : items.filter((i) => i.kind === "assistantMessage").map((i) => i.itemKey),
      },
    };
  }
  if (!held) return { items, held: null };
  /* only a message that was not there when the text was held can be the
     one that replaces it: an earlier narration that happens to begin with
     the same words does not end the hold */
  const prefix = held.content.trimEnd();
  const replaced = items.find(
    (i) =>
      i.kind === "assistantMessage" &&
      !held!.earlier.includes(i.itemKey) &&
      Boolean(i.content) &&
      i.content!.startsWith(prefix),
  );
  if (replaced) return { items, held: null, replacedBy: replaced.itemKey };
  if (live && live.kind === "liveAssistant") {
    return {
      items: items.map((i) => (i === live ? { ...live, content: held.content } : i)),
      held,
    };
  }
  return {
    items: [
      ...items,
      {
        kind: "liveAssistant",
        itemKey: held.itemKey,
        content: held.content,
        reasoning: null,
      },
    ],
    held,
  };
}

/* The keys a transcript draws its turns under. Items stand in for one
   another as a turn settles, each under its own key: the person's message
   is the app's own copy, then the bridge's pending turn, then the saved
   message; the reply is the live tail, then the saved message. Drawn under
   their own keys, React removes one's rows and inserts the other's, and the
   browser lays the page out between the two. So a person's message is drawn
   under its request, whatever stands for it, and the saved reply under the
   key its live tail was drawn under. The bridge names a saved message's
   request by its document id and a pending turn's by its id, so a saved
   message is drawn under a request key of its own. The bridge names every
   turn's live tail alike, so each tail gets a key of its own. */
export type DrawKeys = {
  sessionId: string | null;
  tail: string | null;
  inherited: ReadonlyMap<string, string>;
  tails: number;
};

export const noDrawKeys = (sessionId: string | null): DrawKeys => ({
  sessionId,
  tail: null,
  inherited: new Map(),
  tails: 0,
});

/* `replacedBy` is the message that just ended the hold (`holdLive`). */
export function drawKeys(
  keys: DrawKeys,
  items: RenderedTimelineItem[],
  replacedBy: string | undefined,
  sessionId: string | null,
): DrawKeys {
  let next = keys.sessionId === sessionId ? keys : noDrawKeys(sessionId);
  if (replacedBy && next.tail && !next.inherited.has(replacedBy)) {
    next = {
      ...next,
      tail: null,
      inherited: new Map(next.inherited).set(replacedBy, next.tail),
    };
  }
  if (!next.tail && items.some((i) => i.kind === "liveAssistant")) {
    next = { ...next, tail: `reply-${next.tails + 1}`, tails: next.tails + 1 };
  }
  return next;
}

const requestOf = (item: RenderedTimelineItem) =>
  item.kind === "userMessage"
    ? (item.inputRequestId ?? item.requestId ?? null)
    : item.kind === "pendingUserTurn"
      ? item.requestId
      : null;

export const drawKey = (keys: DrawKeys, item: RenderedTimelineItem): string => {
  const request = requestOf(item);
  if (request) return `turn:${request}`;
  return (
    keys.inherited.get(item.itemKey) ??
    (item.kind === "liveAssistant" && keys.tail ? keys.tail : item.itemKey)
  );
};

/* The message a person sent, as the app holds it until the transcript does. */
export type LocalTurn = {
  sessionId: string;
  requestId: string;
  content: string;
  selectedSkillIds: string[];
  lifecycleState: string | null;
  createdAt: string | null;
};

/* how settled a stand-in for a person's message is: the saved one wins */
const settledness = (item: RenderedTimelineItem) =>
  item.kind === "userMessage" ? 2 : item.itemKey.startsWith("local:") ? 0 : 1;

/**
 * The rows a transcript draws for its turns: one per message a person sent,
 * the most settled of whatever stands for it, with the app's own copy of a
 * message just sent drawn from the moment it is sent, before any live reply
 * that follows it.
 */
export function withSentTurns(
  items: RenderedTimelineItem[],
  local: LocalTurn | null,
  sessionId: string | null,
): RenderedTimelineItem[] {
  const shown =
    local &&
    local.sessionId === sessionId &&
    !items.some((i) => requestOf(i) === local.requestId)
      ? (() => {
          const copy: RenderedTimelineItem = {
            kind: "pendingUserTurn",
            itemKey: `local:${local.requestId}`,
            requestId: local.requestId,
            content: local.content,
            selectedSkillIds: local.selectedSkillIds,
            lifecycleState: local.lifecycleState,
            foldedIntoRequestId: null,
            origin: null,
            createdAt: local.createdAt,
          };
          const tailAt = items.findIndex((i) => i.kind === "liveAssistant");
          return tailAt < 0
            ? [...items, copy]
            : [...items.slice(0, tailAt), copy, ...items.slice(tailAt)];
        })()
      : items;
  const best = new Map<string, RenderedTimelineItem>();
  for (const item of shown) {
    const request = requestOf(item);
    if (!request) continue;
    const held = best.get(request);
    if (!held || settledness(item) > settledness(held)) best.set(request, item);
  }
  if (best.size === shown.filter((i) => requestOf(i)).length) return shown;
  return shown.filter((item) => {
    const request = requestOf(item);
    return !request || best.get(request) === item;
  });
}
