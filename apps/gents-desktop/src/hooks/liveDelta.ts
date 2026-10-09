import {
  isTerminalTurnState,
  type DesktopSessionSnapshot,
  type SessionLiveDeltaView,
} from "@source-inc/gents-desktop-client";

const utf8 = new TextEncoder();

function liveTextHash(value: string) {
  let hash = 0x811c9dc5;
  for (const byte of utf8.encode(value)) {
    hash ^= byte;
    hash = Math.imul(hash, 0x01000193) >>> 0;
  }
  return hash.toString(16).padStart(8, "0");
}

export function sessionLiveDeltaRequest(
  session: DesktopSessionSnapshot,
  requestId: string,
) {
  const revision = session.projectionRevision;
  if (!revision || !session.liveCursor || session.latestRequestId !== requestId)
    return null;
  const live = session.timelineItems.find((item) => item.kind === "liveAssistant");
  const content = live?.content ?? "";
  const reasoning = live?.reasoning ?? "";
  return {
    sessionId: session.sessionId,
    agentDid: session.agentDid,
    requestId,
    baseLiveCursor: session.liveCursor,
    baseContentByteLen: utf8.encode(content).byteLength,
    baseContentHash: liveTextHash(content),
    baseReasoningByteLen: utf8.encode(reasoning).byteLength,
    baseReasoningHash: liveTextHash(reasoning),
  };
}

function applyLiveTextPatch(
  current: string | null | undefined,
  patch: NonNullable<SessionLiveDeltaView["content"]>,
) {
  const base = current ?? "";
  const next =
    patch.mode === "unchanged"
      ? base
      : patch.mode === "append"
        ? `${base}${patch.value}`
        : patch.mode === "replace"
          ? patch.value
          : null;
  if (next == null) return null;
  if (
    utf8.encode(next).byteLength !== patch.byteLen ||
    liveTextHash(next) !== patch.hash
  ) {
    return null;
  }
  return next || null;
}

/** Native adapter of ClientLiveDelta.accepts; identity is encoded by the bridge. */
export function acceptsLiveCursor(
  base: string | null | undefined,
  current: string | null | undefined,
  terminal: boolean,
): boolean {
  return !terminal && base != null && base === current;
}

/** Apply a bridge-checked response suffix without rebuilding historical rows. */
export function applySessionLiveDelta(
  current: DesktopSessionSnapshot,
  delta: SessionLiveDeltaView,
): DesktopSessionSnapshot | null {
  if (
    delta.outcome === "snapshotRequired" ||
    delta.requestId !== current.latestRequestId ||
    !current.projectionRevision ||
    !acceptsLiveCursor(
      current.liveCursor,
      delta.liveCursor,
      (!!delta.turnState && isTerminalTurnState(delta.turnState)) ||
        (!!current.turnState && isTerminalTurnState(current.turnState)),
    ) ||
    delta.revision.storeVersion < current.projectionRevision.storeVersion
  ) {
    return null;
  }
  if (delta.outcome === "unchanged") {
    return {
      ...current,
      turnState: delta.turnState,
      projectionRevision: delta.revision,
    };
  }
  if (delta.outcome !== "delta" || !delta.content || !delta.reasoning) {
    return null;
  }

  const liveIndex = current.timelineItems.findIndex(
    (item) => item.kind === "liveAssistant",
  );
  if (liveIndex < 0) return null;
  const live = current.timelineItems[liveIndex];
  if (live.kind !== "liveAssistant") return null;
  const content = applyLiveTextPatch(live.content, delta.content);
  const reasoning = applyLiveTextPatch(live.reasoning, delta.reasoning);
  if (content === null && delta.content.byteLen > 0) return null;
  if (reasoning === null && delta.reasoning.byteLen > 0) return null;

  const timelineItems = current.timelineItems.slice();
  const liveTailCleared = content === null && reasoning === null;
  if (liveTailCleared) {
    timelineItems.splice(liveIndex, 1);
  } else {
    timelineItems[liveIndex] = {
      kind: "liveAssistant",
      itemKey: timelineItems[liveIndex].itemKey,
      content,
      reasoning,
    };
  }
  return {
    ...current,
    turnState: delta.turnState,
    timelineItems,
    projectionRevision: delta.revision,
  };
}
