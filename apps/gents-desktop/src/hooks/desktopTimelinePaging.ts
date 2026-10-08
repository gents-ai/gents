import type {
  DesktopSessionSnapshot,
  RenderedTimelineItem,
} from "@source-inc/gents-desktop-client";

function timelineItemIdentity(item: RenderedTimelineItem) {
  return `${item.kind}:${item.itemKey}`;
}

function removeMaterializedPendingTurns(items: RenderedTimelineItem[]) {
  const materialized = new Set(
    items.flatMap((item) =>
      item.kind === "userMessage" && item.requestId ? [item.requestId] : [],
    ),
  );
  return items.filter(
    (item) => item.kind !== "pendingUserTurn" || !materialized.has(item.requestId),
  );
}

function sameOptionalStrings(left: string[] | undefined, right: string[] | undefined) {
  if (left === right) return true;
  if (!left || !right || left.length !== right.length) return false;
  return left.every((value, index) => value === right[index]);
}

function timelineItemUnchanged(
  previous: RenderedTimelineItem,
  next: RenderedTimelineItem,
) {
  if (previous.kind !== next.kind || previous.itemKey !== next.itemKey) return false;
  switch (previous.kind) {
    case "userMessage":
      return (
        next.kind === "userMessage" &&
        previous.requestId === next.requestId &&
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
        next.kind === "toolGroup" && JSON.stringify(previous) === JSON.stringify(next)
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

/**
 * Merge an authoritative tip page into any older pages the reader explicitly
 * loaded. Rows inside the incoming tip are replaced, rows before its cursor
 * retain object identity, and stale live-tail rows disappear.
 */
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
  /* The tip is the newest rows, so when a row leaves its end (a live tail
     dropped before the message replacing it lands) it starts a row earlier.
     Rows older than the first one shown would land above the reader and
     move everything under them; they arrive only through paging, which
     holds the reader's place. */
  const firstShown = current.timelineItems[0];
  const shownFrom = firstShown
    ? tip.timelineItems.findIndex(
        (item) => timelineItemIdentity(item) === timelineItemIdentity(firstShown),
      )
    : -1;
  const trimmed = shownFrom > 0;
  const next = {
    ...tip,
    timelineItems: trimmed ? tip.timelineItems.slice(shownFrom) : tip.timelineItems,
    timelinePage: trimmed
      ? { ...tip.timelinePage, hasOlder: true, oldestItemKey: firstShown!.itemKey }
      : tip.timelinePage,
  };

  const firstIncoming = next.timelineItems[0];
  const overlapIndex = firstIncoming
    ? current.timelineItems.findIndex(
        (item) => timelineItemIdentity(item) === timelineItemIdentity(firstIncoming),
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
  const preserveLoadedOlderState = retainedPrefix.length > 0 || hasSyntheticOlderCursor;

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
        : (timelineItems[0]?.itemKey ?? null),
    },
  };
}

/** Merge an older page without allowing its metadata to regress the live tip. */
export function mergeOlderSessionTimelinePage(
  current: DesktopSessionSnapshot | null,
  older: DesktopSessionSnapshot,
): DesktopSessionSnapshot {
  if (!current || current.sessionId !== older.sessionId || !older.timelinePage) {
    return current ?? older;
  }
  const currentIdentities = new Set(current.timelineItems.map(timelineItemIdentity));
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
      // A durable page can contain only rows the timeline intentionally hides.
      // Preserve the bridge's synthetic cursor so the next query advances.
      oldestItemKey:
        prefix.length > 0
          ? (timelineItems[0]?.itemKey ?? null)
          : older.timelinePage.oldestItemKey,
      newestItemKey: timelineItems[timelineItems.length - 1]?.itemKey ?? null,
      queryCount: (currentPage.queryCount ?? 0) + (older.timelinePage.queryCount ?? 0),
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
