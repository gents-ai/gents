import type { DesktopClientUpdatedListenerFactory } from "@source-inc/gents-desktop-client";
import { listenToDesktopClientUpdates } from "@source-inc/gents-desktop-client";

import { clientRunning } from "./clientStore";
import { readSession } from "./sessionStore";
import type { DesktopApp } from "./desktopApp";
import { createProjectionController } from "./projectionController";
import { logShellEvent } from "./clientLifecycle";
import { desktopUpdateRefreshScope } from "./projectionController";
import { timingConfig } from "./timing";
import { errorText } from "../lib/errorText";
import { activityExpiry } from "./activityExpiry";

export function startClientObservation(
  { api, stores, view, actions, lifecycle, trackedRequestId }: DesktopApp,
  listenToUpdates: DesktopClientUpdatedListenerFactory,
): () => void {
  const selection = stores.selection;
  const { refreshSession, refreshSessionLiveDelta, refreshSnapshot } = actions;

  function run() {
    let stopped = false;
    let unlisten: (() => void) | undefined;
    const reportListenerError = (listenerError: unknown) => {
      if (stopped) return;
      const message = errorText(listenerError);
      logShellEvent(`desktop update listener failed: ${message}`);
      lifecycle.setError(message);
    };
    const controller = createProjectionController({
      currentSessionId: () => selection.getState().sessionId,
      refreshSnapshot,
      refreshSession,
      refreshSessionLiveDelta,
      onError: reportListenerError,
    });
    void listenToDesktopClientUpdates(
      async (event) => {
        if (stopped) return;
        const scope = desktopUpdateRefreshScope(
          event.reason,
          selection.getState().sessionId,
          trackedRequestId(),
        );
        await controller.request(scope);
      },
      reportListenerError,
      listenToUpdates,
    )
      .then((cleanup) => {
        if (stopped) cleanup();
        else unlisten = cleanup;
      })
      .catch(reportListenerError);

    let published = 0;
    const publishSelection = () => {
      const mine = ++published;
      void api.setSelectedNode(selection.getState().nodeDid).catch((err) => {
        if (!stopped && mine === published) lifecycle.setError(String(err));
      });
    };

    const pollMs = timingConfig().activeSessionPollMs;
    let polled: string | null = null;
    let pollTimer: ReturnType<typeof setTimeout> | undefined;
    let activityTimer: ReturnType<typeof setTimeout> | undefined;
    let activityKey: string | null = null;
    const followActivityExpiry = () => {
      const session = readSession(stores.session) as
        | (NonNullable<ReturnType<typeof readSession>> & {
            executionActivityExpiresAt?: number | null;
          })
        | null;
      const selected = selection.getState();
      const expiry = activityExpiry(
        session,
        selected.sessionId,
        selected.nodeDid,
        Date.now(),
      );
      const key = expiry?.key ?? null;
      if (key === activityKey) return;
      activityKey = key;
      clearTimeout(activityTimer);
      activityTimer = undefined;
      if (key !== null && expiry) {
        activityTimer = setTimeout(() => {
          if (!stopped && activityKey === key) void controller.request("session");
        }, expiry.delay);
      }
    };
    const followPoll = () => {
      const sessionId = selection.getState().sessionId;
      const requestId = view.getState().trackedRequestId;
      const key =
        sessionId && requestId && pollMs !== null ? `${sessionId}\0${requestId}` : null;
      if (key === polled) return;
      polled = key;
      clearTimeout(pollTimer);
      pollTimer = undefined;
      if (key === null || pollMs === null) return;
      const nextPollMs = () =>
        readSession(stores.session)?.liveCursor
          ? Math.min(250, pollMs)
          : (actions.sessionReadGapMs() ?? pollMs);
      const poll = async () => {
        await controller.request("sessionDelta");
        if (!stopped && polled === key) pollTimer = setTimeout(poll, nextPollMs());
      };
      pollTimer = setTimeout(poll, nextPollMs());
    };

    publishSelection();
    void controller.request("session");
    followPoll();
    followActivityExpiry();
    const unsubscribeSession = stores.session.subscribe(followActivityExpiry);
    const unsubscribeSelection = selection.subscribe((state, prev) => {
      if (state.nodeDid !== prev.nodeDid) publishSelection();
      if (state.nodeDid !== prev.nodeDid || state.sessionId !== prev.sessionId) {
        void controller.request("session");
        followPoll();
        followActivityExpiry();
      }
    });
    const unsubscribeTracked = view.subscribe((state, prev) => {
      if (state.trackedRequestId === prev.trackedRequestId) return;
      void controller.request("session");
      followPoll();
    });

    const refreshForegroundState = () => {
      if (document.visibilityState !== "hidden") void controller.request("full");
    };
    const onVisibilityChange = () => {
      if (document.visibilityState === "visible") refreshForegroundState();
    };
    document.addEventListener("visibilitychange", onVisibilityChange);
    window.addEventListener("focus", refreshForegroundState);

    return () => {
      stopped = true;
      controller.dispose();
      actions.invalidateSessionReads();
      clearTimeout(pollTimer);
      clearTimeout(activityTimer);
      unsubscribeSession();
      unsubscribeSelection();
      unsubscribeTracked();
      document.removeEventListener("visibilitychange", onVisibilityChange);
      window.removeEventListener("focus", refreshForegroundState);
      unlisten?.();
    };
  }

  let stopRun: (() => void) | null = null;
  const follow = () => {
    const running = clientRunning(stores.client.getState());
    if (running && !stopRun) stopRun = run();
    else if (!running && stopRun) {
      stopRun();
      stopRun = null;
    }
  };
  const unsubscribeClient = stores.client.subscribe(follow);
  follow();
  return () => {
    unsubscribeClient();
    stopRun?.();
    stopRun = null;
  };
}
