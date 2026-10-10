import type { DesktopClientUpdatedListenerFactory } from "@source-inc/gents-desktop-client";
import { listenToDesktopClientUpdates } from "@source-inc/gents-desktop-client";

import { isMacTauriShell } from "../lib/shellPlatform";
import { clientRunning } from "./clientStore";
import { readSession } from "./sessionStore";
import type { DesktopApp } from "./desktopApp";
import { createProjectionController } from "./projectionController";
import { logShellEvent } from "./clientLifecycle";
import { desktopUpdateRefreshScope } from "./projectionController";
import { timingConfig } from "./timing";

/**
 * How the desktop follows the client while it runs, as reactions to the
 * stores rather than to renders: one projection controller, one bridge
 * listener and the foreground reads for each run of the client; a session
 * read and the selected node told to the host whenever the selection moves;
 * and the live-delta poll while a request in the selected session is
 * tracked. Returns the way to stop.
 *
 * Nothing runs until the client does: starting the controller during
 * configuration bootstrap races the lifecycle's authoritative snapshot read
 * and can clear its failure while leaving startupPhase at
 * configuration-error.
 */
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
      const message =
        listenerError instanceof Error ? listenerError.message : String(listenerError);
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

    /* the host narrows its observation to the selected node; only the newest
       publish may report a failure */
    let published = 0;
    const publishSelection = () => {
      const mine = ++published;
      void api.setSelectedNode(selection.getState().nodeDid).catch((err) => {
        if (!stopped && mine === published) lifecycle.setError(String(err));
      });
    };

    /* the live cursor, polled while a request in the selected session is
       tracked; a continuity failure promotes itself to one full bounded
       projection */
    const pollMs = timingConfig().activeSessionPollMs;
    let polled: string | null = null;
    let pollTimer: ReturnType<typeof setTimeout> | undefined;
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
        readSession(stores.session)?.liveCursor ? Math.min(250, pollMs) : pollMs;
      const poll = async () => {
        await controller.request("sessionDelta");
        if (!stopped && polled === key) pollTimer = setTimeout(poll, nextPollMs());
      };
      pollTimer = setTimeout(poll, nextPollMs());
    };

    publishSelection();
    void controller.request("session");
    followPoll();
    const unsubscribeSelection = selection.subscribe((state, prev) => {
      if (state.nodeDid !== prev.nodeDid) publishSelection();
      if (state.nodeDid !== prev.nodeDid || state.sessionId !== prev.sessionId) {
        void controller.request("session");
        followPoll();
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
    /* closing sibling windows returns focus to the surviving view: the host
       can narrow its observation again once it is the only one */
    const mac = isMacTauriShell();
    if (mac) window.addEventListener("focus", publishSelection);

    return () => {
      stopped = true;
      controller.dispose();
      actions.invalidateSessionReads();
      clearTimeout(pollTimer);
      unsubscribeSelection();
      unsubscribeTracked();
      document.removeEventListener("visibilitychange", onVisibilityChange);
      window.removeEventListener("focus", refreshForegroundState);
      if (mac) window.removeEventListener("focus", publishSelection);
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
