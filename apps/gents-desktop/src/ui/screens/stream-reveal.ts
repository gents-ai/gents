import type {
  RenderedTimelineItem,
  RequestLifecycleState,
} from "@source-inc/gents-desktop-client";

const MIN_CPS = 45;
const MIN_GAP_MS = 40;
const MAX_GAP_MS = 2_000;
const DRAIN_OVER_GAP = 1.5;
const MAX_BACKLOG = 1_200;

export type RevealState = {
  shown: number;
  carry: number;
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
    cps: Math.max(MIN_CPS, ((targetLen - shown) / (1_000 * DRAIN_OVER_GAP)) * 1_000),
    targetLen,
    lastGrowthAt: null,
    gapMs: 1_000,
  };
}

export function stepReveal(
  state: RevealState,
  targetLen: number,
  now: number,
  dtMs: number,
): RevealState {
  let { shown, carry, cps, lastGrowthAt, gapMs } = state;
  if (targetLen < shown) {
    shown = targetLen;
    carry = 0;
  }
  if (targetLen > state.targetLen) {
    if (lastGrowthAt !== null) {
      const gap = Math.min(MAX_GAP_MS, Math.max(MIN_GAP_MS, now - lastGrowthAt));
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

export function closeOpenFence(text: string): string {
  const fences = text
    .split("\n")
    .filter((line) => line.trimStart().startsWith("```")).length;
  return fences % 2 === 1 ? `${text}\n\`\`\`` : text;
}

export type HeldLive = {
  sessionId: string | null;
  itemKey: string;
  content: string;
  reasoning: string | null;
  earlier: string[];
};

export function holdLive(
  items: RenderedTimelineItem[],
  held: HeldLive | null,
  sessionId: string | null = null,
): {
  items: RenderedTimelineItem[];
  held: HeldLive | null;
  replacedBy?: string;
} {
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

const requestOf = (item: RenderedTimelineItem): string | null => {
  if (item.kind === "pendingUserTurn") return item.requestId;
  if (item.kind === "userMessage")
    return item.inputRequestId ?? (item.ownsTurn ? (item.requestId ?? null) : null);
  return null;
};

/** The key a person's message is drawn under, from the request it made:
    the same for the app's own copy, the saved row and a folded input. */
export const turnKey = (requestId: string) => `turn:${requestId}`;

export type SentTurn = { sessionId: string; requestId: string; queued: boolean };
export type FoldedInput = { requestId: string; foldedIntoRequestId: string };

/**
 * The turn the view goes to after a send from here, by key, or null while
 * there is none: the newest send whose turn has begun. A message sent
 * with nothing running begins as it is drawn; one queued behind a running
 * turn has begun when the session makes it the latest request, or lists
 * it folded into one. Messages folded into the same request begin
 * together, so the view goes to the first of them.
 */
export function sentTurnKey(
  sent: readonly SentTurn[],
  sessionId: string | null,
  latestRequestId: string | null,
  folded: readonly FoldedInput[],
): string | null {
  const foldedInto = (turn: SentTurn) =>
    folded.find((input) => input.requestId === turn.requestId)?.foldedIntoRequestId ??
    null;
  const begun = sent.filter(
    (turn) =>
      turn.sessionId === sessionId &&
      (!turn.queued || turn.requestId === latestRequestId || foldedInto(turn) !== null),
  );
  const newest = begun[begun.length - 1];
  if (!newest) return null;
  const into = foldedInto(newest);
  const first =
    into === null ? newest : begun.find((turn) => foldedInto(turn) === into)!;
  return turnKey(first.requestId);
}

export const drawKey = (keys: DrawKeys, item: RenderedTimelineItem): string => {
  const request = requestOf(item);
  if (request) return turnKey(request);
  return (
    keys.inherited.get(item.itemKey) ??
    (item.kind === "liveAssistant" && keys.tail ? keys.tail : item.itemKey)
  );
};

export type LocalTurn = {
  sessionId: string;
  requestId: string;
  content: string;
  selectedSkillIds: string[];
  lifecycleState: RequestLifecycleState | null;
  createdAt: string | null;
};

/** The app's copy of a sent message, kept while no row stands for its
    request; the caller bounds how long, for a row that never comes. */
export function lingerSentTurn(
  items: RenderedTimelineItem[],
  local: LocalTurn | null,
  last: LocalTurn | null,
): LocalTurn | null {
  if (local) return local;
  if (!last || items.some((item) => requestOf(item) === last.requestId)) return null;
  return last;
}

const settledness = (item: RenderedTimelineItem) =>
  item.kind === "userMessage" ? 2 : item.itemKey.startsWith("local:") ? 0 : 1;

/** The rows drawn for sent messages: one per message, the most settled of
    whatever stands for it, the app's own copy drawn before any live reply.
    A message queued behind a running turn waits in the tray instead. */
export function withSentTurns(
  items: RenderedTimelineItem[],
  local: LocalTurn | null,
  sessionId: string | null,
  queued = false,
): RenderedTimelineItem[] {
  const shown =
    local &&
    !queued &&
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
