import { turnKey, type SentTurn } from "../../hooks/sentTurns";
import type {
  AttachmentView,
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

export const requestOf = (item: RenderedTimelineItem): string | null => {
  if (item.kind === "pendingUserTurn") return item.requestId;
  if (item.kind === "userMessage")
    return item.inputRequestId ?? (item.ownsTurn ? (item.requestId ?? null) : null);
  return null;
};

export type FoldedInput = { requestId: string; foldedIntoRequestId: string };

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
  return first.taken ? null : turnKey(first.requestId);
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
  attachments?: AttachmentView[] | null;
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

export function withSentTurns(
  items: RenderedTimelineItem[],
  local: LocalTurn | null,
  sessionId: string | null,
  queued = false,
  kept: readonly RenderedTimelineItem[] = [],
): RenderedTimelineItem[] {
  const copies: RenderedTimelineItem[] = [...kept];
  if (
    local &&
    !queued &&
    local.sessionId === sessionId &&
    !items.some((i) => requestOf(i) === local.requestId)
  )
    copies.push({
      kind: "pendingUserTurn",
      itemKey: `local:${local.requestId}`,
      requestId: local.requestId,
      content: local.content,
      attachments: local.attachments,
      selectedSkillIds: local.selectedSkillIds,
      lifecycleState: local.lifecycleState,
      foldedIntoRequestId: null,
      origin: null,
      createdAt: local.createdAt,
    });
  const sentAt = (item: RenderedTimelineItem) =>
    (item.kind === "pendingUserTurn" && item.createdAt && Date.parse(item.createdAt)) ||
    0;
  copies.sort((a, b) => sentAt(a) - sentAt(b));
  const tailAt = items.findIndex((i) => i.kind === "liveAssistant");
  const shown = !copies.length
    ? items
    : tailAt < 0
      ? [...items, ...copies]
      : [...items.slice(0, tailAt), ...copies, ...items.slice(tailAt)];
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

/** How long a message whose content is still arriving is held back before
    it is drawn with its placeholder. Most rows complete well within it, so
    they appear once, whole, instead of flashing a placeholder and jumping. */
export const SETTLE_MS = 1_500;

/** The rows to draw now: a message still loading is left out until it is
    complete or has been loading for `graceMs`; inputs this device sent are
    always drawn, since their own copy was replaced by the row. `firstSeen`
    keeps when each held row was first seen loading. */
/** An input's text, once this device has shown it, stays shown: a later read
    that has the input's row but not yet its message (a reload, the request
    turning from queued to running) would otherwise swap the text for "still
    syncing" and back. `known` holds the text by request, across renders. */
export function keepKnownInputs(
  items: RenderedTimelineItem[],
  known: Map<string, string>,
  kept = 128,
): RenderedTimelineItem[] {
  let changed = false;
  const out = items.map((item) => {
    if (item.kind !== "userMessage" && item.kind !== "pendingUserTurn") return item;
    const request = requestOf(item);
    if (!request) return item;
    const loading =
      item.kind === "userMessage" && item.reconstruction?.state === "loading";
    if (!loading) {
      if (item.content?.trim()) {
        known.delete(request);
        known.set(request, item.content);
        if (known.size > kept) known.delete(known.keys().next().value!);
      }
      return item;
    }
    const text = known.get(request);
    if (text === undefined) return item;
    changed = true;
    return { ...item, content: text, reconstruction: { state: "ready" as const } };
  });
  return changed ? out : items;
}

const LIVE_PREFIX = "live-assistant-";

/** A reply's live text stays shown until its saved message can be drawn: when
    the next turn starts, the finished reply's live row goes, and its saved
    message may still be loading for a second or two (its last pieces in
    flight), which drew "Loading message" in between. `live` holds the last
    live text by request, across renders; only the newest saved reply of that
    request takes it, since a turn that used tools has several. */
export function keepLiveReplies(
  items: RenderedTimelineItem[],
  live: Map<string, string>,
  kept = 32,
): RenderedTimelineItem[] {
  for (const item of items) {
    if (item.kind !== "liveAssistant" || !item.itemKey.startsWith(LIVE_PREFIX))
      continue;
    if (!item.content?.trim()) continue;
    const request = item.itemKey.slice(LIVE_PREFIX.length);
    live.delete(request);
    live.set(request, item.content);
    if (live.size > kept) live.delete(live.keys().next().value!);
  }
  const newest = new Map<string, number>();
  items.forEach((item, index) => {
    if (item.kind === "assistantMessage" && item.requestId)
      newest.set(item.requestId, index);
  });
  let changed = false;
  const out = items.map((item, index) => {
    if (item.kind !== "assistantMessage" || !item.requestId) return item;
    const text = live.get(item.requestId);
    if (text === undefined) return item;
    if (item.reconstruction.state !== "loading") {
      if (item.reconstruction.state === "ready") live.delete(item.requestId);
      return item;
    }
    if (newest.get(item.requestId) !== index) return item;
    changed = true;
    return { ...item, content: text, reconstruction: { state: "ready" as const } };
  });
  return changed ? out : items;
}

export type SeenReplies = {
  sessionId: string | null;
  keys: Set<string>;
  primed: boolean;
};

export const noSeenReplies = (sessionId: string | null): SeenReplies => ({
  sessionId,
  keys: new Set(),
  primed: false,
});

/** The saved replies that arrived while this session's transcript was drawn:
    a reply not among the rows first drawn, and placed after every row seen so
    far. Such a reply was never shown live (the model answered faster than
    the screen drew), so it types in rather than popping in whole. Rows of an
    older page are placed before rows already seen, and are not new. */
export function arrivedReplies(
  items: RenderedTimelineItem[],
  seen: SeenReplies,
  sessionId: string | null,
): { seen: SeenReplies; arrived: string[] } {
  if (seen.sessionId !== sessionId) seen = noSeenReplies(sessionId);
  if (!seen.primed) {
    if (items.length === 0) return { seen, arrived: [] };
    return {
      seen: { sessionId, keys: new Set(items.map((i) => i.itemKey)), primed: true },
      arrived: [],
    };
  }
  let lastSeen = -1;
  items.forEach((item, index) => {
    if (seen.keys.has(item.itemKey)) lastSeen = index;
  });
  const arrived: string[] = [];
  let keys = seen.keys;
  items.forEach((item, index) => {
    if (keys.has(item.itemKey)) return;
    if (keys === seen.keys) keys = new Set(seen.keys);
    keys.add(item.itemKey);
    if (
      index > lastSeen &&
      item.kind === "assistantMessage" &&
      item.reconstruction.state === "ready"
    )
      arrived.push(item.itemKey);
  });
  return { seen: keys === seen.keys ? seen : { ...seen, keys }, arrived };
}

export function settledRows(
  items: RenderedTimelineItem[],
  firstSeen: Map<string, number>,
  now: number,
  sentHere: ReadonlySet<string>,
  graceMs = SETTLE_MS,
): { items: RenderedTimelineItem[]; nextAt: number | null } {
  const loading = new Set<string>();
  let nextAt: number | null = null;
  const shown = items.filter((item) => {
    if (item.kind !== "userMessage" && item.kind !== "assistantMessage") return true;
    if (item.reconstruction?.state !== "loading") return true;
    const request = requestOf(item);
    if (request && sentHere.has(request)) return true;
    loading.add(item.itemKey);
    const first = firstSeen.get(item.itemKey) ?? now;
    firstSeen.set(item.itemKey, first);
    if (now - first >= graceMs) return true;
    nextAt = nextAt === null ? first + graceMs : Math.min(nextAt, first + graceMs);
    return false;
  });
  for (const key of [...firstSeen.keys()]) if (!loading.has(key)) firstSeen.delete(key);
  return { items: shown.length === items.length ? items : shown, nextAt };
}
