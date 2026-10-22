import type { DesktopSessionSnapshot, RenderedTimelineItem } from "./types.js";

function timelineItemIdentity(item: RenderedTimelineItem) {
  return `${item.kind}:${item.itemKey}`;
}

/** A pending turn whose input already has its saved row is not drawn: one
 *  message is one row, wherever the two were read from. */
export function removeMaterializedPendingTurns(items: RenderedTimelineItem[]) {
  const materialized = new Set(
    items.flatMap((item) =>
      item.kind === "userMessage" || item.kind === "automatedInput"
        ? [
            item.inputRequestId ??
              (item.kind === "userMessage" && item.ownsTurn
                ? item.requestId
                : null),
          ].filter((id): id is string => Boolean(id))
        : [],
    ),
  );
  return items.filter(
    (item) =>
      item.kind !== "pendingUserTurn" || !materialized.has(item.requestId),
  );
}

function sameOptionalStrings(
  left: string[] | undefined,
  right: string[] | undefined,
) {
  if (left === right) return true;
  if (!left || !right || left.length !== right.length) return false;
  return left.every((value, index) => value === right[index]);
}

function timelineItemUnchanged(
  previous: RenderedTimelineItem,
  next: RenderedTimelineItem,
) {
  if (previous.kind !== next.kind || previous.itemKey !== next.itemKey)
    return false;
  switch (previous.kind) {
    case "userMessage":
      return (
        next.kind === "userMessage" &&
        previous.requestId === next.requestId &&
        previous.ownsTurn === next.ownsTurn &&
        previous.inputRequestId === next.inputRequestId &&
        previous.sequence === next.sequence &&
        previous.content === next.content &&
        previous.timestamp === next.timestamp &&
        previous.reconstruction.state === next.reconstruction.state &&
        previous.reconstruction.error === next.reconstruction.error &&
        previous.reconstruction.deniedDependencyDocId ===
          next.reconstruction.deniedDependencyDocId
      );
    case "assistantMessage":
      return (
        next.kind === "assistantMessage" &&
        previous.sequence === next.sequence &&
        previous.content === next.content &&
        previous.reasoning === next.reasoning &&
        previous.timestamp === next.timestamp &&
        previous.reconstruction.state === next.reconstruction.state &&
        previous.reconstruction.error === next.reconstruction.error &&
        previous.reconstruction.deniedDependencyDocId ===
          next.reconstruction.deniedDependencyDocId
      );
    case "requestContextReductionSummary":
      return (
        next.kind === "requestContextReductionSummary" &&
        previous.sequence === next.sequence &&
        previous.requestDocId === next.requestDocId &&
        previous.requestId === next.requestId &&
        previous.reductionCount === next.reductionCount &&
        previous.compactedCount === next.compactedCount &&
        previous.repairedCount === next.repairedCount &&
        previous.latestEstimatedInputTokens ===
          next.latestEstimatedInputTokens &&
        previous.latestTimestamp === next.latestTimestamp
      );
    case "sessionCompaction":
      return (
        next.kind === "sessionCompaction" &&
        previous.sequence === next.sequence &&
        previous.messagesCompacted === next.messagesCompacted &&
        previous.originalTokens === next.originalTokens &&
        previous.compactedTokens === next.compactedTokens &&
        previous.timestamp === next.timestamp
      );
    case "pendingUserTurn":
      return (
        next.kind === "pendingUserTurn" &&
        previous.requestId === next.requestId &&
        previous.content === next.content &&
        previous.lifecycleState === next.lifecycleState &&
        previous.foldedIntoRequestId === next.foldedIntoRequestId &&
        JSON.stringify(previous.origin) === JSON.stringify(next.origin) &&
        previous.createdAt === next.createdAt &&
        sameOptionalStrings(previous.selectedSkillIds, next.selectedSkillIds)
      );
    case "automatedInput":
      return (
        next.kind === "automatedInput" &&
        previous.requestId === next.requestId &&
        previous.inputRequestId === next.inputRequestId &&
        previous.sequence === next.sequence &&
        previous.content === next.content &&
        previous.timestamp === next.timestamp &&
        JSON.stringify(previous.origin) === JSON.stringify(next.origin) &&
        previous.reconstruction.state === next.reconstruction.state &&
        previous.reconstruction.error === next.reconstruction.error &&
        previous.reconstruction.deniedDependencyDocId ===
          next.reconstruction.deniedDependencyDocId
      );
    case "liveAssistant":
      return (
        next.kind === "liveAssistant" &&
        previous.content === next.content &&
        previous.reasoning === next.reasoning
      );
    case "toolGroup":
      return (
        next.kind === "toolGroup" &&
        JSON.stringify(previous) === JSON.stringify(next)
      );
  }
}

function reuseUnchangedTimelineItems(
  previous: RenderedTimelineItem[],
  next: RenderedTimelineItem[],
) {
  const previousByIdentity = new Map(
    previous.map((item) => [timelineItemIdentity(item), item]),
  );
  return next.map((item) => {
    const existing = previousByIdentity.get(timelineItemIdentity(item));
    return existing && timelineItemUnchanged(existing, item) ? existing : item;
  });
}

function hasDurableSequence(item: RenderedTimelineItem): boolean {
  switch (item.kind) {
    case "userMessage":
    case "assistantMessage":
    case "requestContextReductionSummary":
    case "sessionCompaction":
    case "automatedInput":
      return item.sequence != null;
    case "toolGroup":
      return item.messageSequence != null;
    case "pendingUserTurn":
    case "liveAssistant":
      return false;
  }
}

export function mergeSessionTipSnapshot(
  current: DesktopSessionSnapshot | null,
  tip: DesktopSessionSnapshot,
): DesktopSessionSnapshot {
  if (
    !current ||
    current.sessionId !== tip.sessionId ||
    !tip.timelinePage ||
    tip.timelinePage.hasNewer
  ) {
    return tip;
  }
  const firstShown = current.timelineItems.find(hasDurableSequence);
  const shownFrom = firstShown
    ? tip.timelineItems.findIndex(
        (item) =>
          timelineItemIdentity(item) === timelineItemIdentity(firstShown),
      )
    : -1;
  const trimmed = shownFrom > 0;
  const next = {
    ...tip,
    timelineItems: trimmed
      ? tip.timelineItems.slice(shownFrom)
      : tip.timelineItems,
    timelinePage: trimmed
      ? {
          ...tip.timelinePage,
          hasOlder: true,
          oldestItemKey: firstShown!.itemKey,
        }
      : tip.timelinePage,
  };

  const firstIncoming = next.timelineItems[0];
  const overlapIndex = firstIncoming
    ? current.timelineItems.findIndex(
        (item) =>
          timelineItemIdentity(item) === timelineItemIdentity(firstIncoming),
      )
    : -1;
  const retainedPrefix =
    next.timelinePage.hasOlder && overlapIndex >= 0
      ? current.timelineItems.slice(0, overlapIndex)
      : [];
  const timelineItems = reuseUnchangedTimelineItems(
    current.timelineItems,
    removeMaterializedPendingTurns([...retainedPrefix, ...next.timelineItems]),
  );

  const currentPage = current.timelinePage ?? next.timelinePage;
  const currentFirstVisibleKey = current.timelineItems[0]?.itemKey ?? null;
  const hasSyntheticOlderCursor =
    overlapIndex >= 0 &&
    currentPage.oldestItemKey != null &&
    currentPage.oldestItemKey !== currentFirstVisibleKey;
  const preserveLoadedOlderState =
    retainedPrefix.length > 0 || hasSyntheticOlderCursor;

  return {
    ...next,
    timelineItems,
    timelinePage: {
      ...next.timelinePage,
      pageItems: timelineItems.length,
      hasOlder: preserveLoadedOlderState
        ? currentPage.hasOlder
        : next.timelinePage.hasOlder,
      oldestItemKey: preserveLoadedOlderState
        ? currentPage.oldestItemKey
        : next.timelinePage.oldestItemKey,
    },
  };
}

/** An older page joined in front of a snapshot: the stitching a client
 *  that pages the session snapshot itself uses. The desktop reads history
 *  by message number instead (`fetchSessionRows`). */
export function mergeOlderSessionTimelinePage(
  current: DesktopSessionSnapshot | null,
  older: DesktopSessionSnapshot,
): DesktopSessionSnapshot {
  if (
    !current ||
    current.sessionId !== older.sessionId ||
    !older.timelinePage
  ) {
    return current ?? older;
  }
  const currentIdentities = new Set(
    current.timelineItems.map(timelineItemIdentity),
  );
  const prefix = older.timelineItems.filter(
    (item) => !currentIdentities.has(timelineItemIdentity(item)),
  );
  const timelineItems = removeMaterializedPendingTurns([
    ...prefix,
    ...current.timelineItems,
  ]);
  const currentPage = current.timelinePage ?? older.timelinePage;
  const totalItemsExact =
    (currentPage.totalItemsExact ?? true) &&
    (older.timelinePage.totalItemsExact ?? true);
  return {
    ...current,
    timelineItems,
    timelinePage: {
      ...currentPage,
      totalItems: totalItemsExact
        ? Math.max(currentPage.totalItems, older.timelinePage.totalItems)
        : -1,
      totalItemsExact,
      pageItems: timelineItems.length,
      hasOlder: older.timelinePage.hasOlder,
      hasNewer: false,
      oldestItemKey: older.timelinePage.oldestItemKey,
      newestItemKey: timelineItems[timelineItems.length - 1]?.itemKey ?? null,
      queryCount:
        (currentPage.queryCount ?? 0) + (older.timelinePage.queryCount ?? 0),
      queriedRows:
        (currentPage.queriedRows ?? 0) + (older.timelinePage.queriedRows ?? 0),
      messageQueryLimit: Math.max(
        currentPage.messageQueryLimit ?? 0,
        older.timelinePage.messageQueryLimit ?? 0,
      ),
      toolCallQueryLimit: Math.max(
        currentPage.toolCallQueryLimit ?? 0,
        older.timelinePage.toolCallQueryLimit ?? 0,
      ),
    },
  };
}
