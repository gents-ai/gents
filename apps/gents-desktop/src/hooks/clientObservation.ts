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

export function startClientObservation(
  { api, stores, view, actions, lifecycle, trackedRequestId }: DesktopApp,
  listenToUpdates: DesktopClientUpdatedListenerFactory,
): () => void {
  const selection = stores.selection;
  const { refreshSession, readSessionUpdate, refreshSnapshot } = actions;
  /* the selected session shows no snapshot: it waits on its node, or its
     read has not resolved yet */
  const selectedSessionUnresolved = () => {
    const { sessionId, nodeDid } = selection.getState();
    const open = readSession(stores.session);
    return !(
      open &&
      open.sessionId === sessionId &&
      (!nodeDid || !open.nodeDid || open.nodeDid === nodeDid)
    );
  };

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
      readSessionUpdate,
      onError: reportListenerError,
    });
    /* Store and pairing-health events drive every read of a session whose
       rows replicate here. */
    void listenToDesktopClientUpdates(
      async (event) => {
        if (stopped) return;
        const scope = desktopUpdateRefreshScope(
          event.reason,
          selection.getState().sessionId,
          trackedRequestId(),
          selectedSessionUnresolved(),
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

    const hidden = () => document.visibilityState === "hidden";

    /* A session read through the node's operator access has no replica here,
       so no store event reports its changes. While it has a request open and
       the window shows it, it is read at the session read gap, live text
       sooner, until the node's runtime gains an event source of its own. */
    let operatorTimer: ReturnType<typeof setTimeout> | undefined;
    const scheduleOperatorRead = () => {
      clearTimeout(operatorTimer);
      operatorTimer = undefined;
      const open = readSession(stores.session);
      const gap = timingConfig().sessionReadGapMs;
      if (stopped || hidden() || gap === null || !trackedRequestId()) return;
      if (
        !open?.readThroughOperator ||
        open.sessionId !== selection.getState().sessionId
      )
        return;
      operatorTimer = setTimeout(
        async () => {
          operatorTimer = undefined;
          await controller.request("sessionDelta");
          scheduleOperatorRead();
        },
        open.liveCursor ? Math.min(250, gap) : gap,
      );
    };
    const unsubscribeOperator = stores.session.subscribe((state, prev) => {
      if (
        state.session?.sessionId !== prev.session?.sessionId ||
        state.session?.readThroughOperator !== prev.session?.readThroughOperator ||
        (operatorTimer === undefined && state.session?.readThroughOperator)
      )
        scheduleOperatorRead();
    });

    publishSelection();
    void controller.request("session");
    const unsubscribeSelection = selection.subscribe((state, prev) => {
      if (state.nodeDid !== prev.nodeDid) publishSelection();
      if (state.nodeDid !== prev.nodeDid || state.sessionId !== prev.sessionId) {
        void controller.request("session");
        scheduleOperatorRead();
      }
    });
    const unsubscribeTracked = view.subscribe((state, prev) => {
      if (state.trackedRequestId === prev.trackedRequestId) return;
      void controller.request("session");
      scheduleOperatorRead();
    });

    /* showing the window reads it in full */
    const refreshForegroundState = () => {
      if (hidden()) {
        scheduleOperatorRead();
        return;
      }
      void controller.request("full").then(scheduleOperatorRead);
    };
    document.addEventListener("visibilitychange", refreshForegroundState);
    window.addEventListener("focus", refreshForegroundState);

    return () => {
      stopped = true;
      controller.dispose();
      actions.invalidateSessionReads();
      clearTimeout(operatorTimer);
      unsubscribeOperator();
      unsubscribeSelection();
      unsubscribeTracked();
      document.removeEventListener("visibilitychange", refreshForegroundState);
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
