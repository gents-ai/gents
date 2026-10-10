import type {
  DesktopApiAdapter,
  ManagedServerAuthorityInput,
  ManagedServerStatus,
} from "@source-inc/gents-desktop-client";

import {
  ensureManagedRuntimeServing,
  waitForManagedRuntimePairing,
} from "../lib/managedRuntimeReadiness";
import {
  awaitManagedServerSettled,
  observeManagedServerOperation,
  type ManagedServerWait,
} from "../lib/managedServerStartup";
import { newestWins } from "../lib/reads";
import type { ClientStore } from "./clientStore";
import {
  localServer,
  type LocalServerOperation,
  type LocalServerStore,
} from "./localServerStore";
import { restoreManagedServer } from "./managedServerLifecycle";

type LocalServerParams = {
  api: DesktopApiAdapter;
  store: LocalServerStore;
  client: ClientStore;
};

/**
 * The one owner of the OS-managed local node service in this window, shared
 * by startup, the tray, setup and the node screen. Its status and the wait
 * an operation is in are held once in the store. Operations run one at a
 * time: matching commands share a result, while different inputs queue.
 * Readiness and restore calls retain their individual wait options, so the
 * tray and a screen cannot start or stop the service over each other.
 */
export function createLocalServer({ api, store, client }: LocalServerParams) {
  /* only the newest read is shown: a read begun before an operation may
     answer after it with the status it replaced */
  const reads = newestWins();
  const waits = newestWins();
  let watchers = 0;
  let running: { key: string | symbol; done: Promise<unknown> } | null = null;

  const publishWait = (wait: ManagedServerWait | null) =>
    localServer.waiting(store, wait);
  const settled = (status: ManagedServerStatus) => {
    reads.supersede();
    localServer.read(store, status);
    return status;
  };

  function refreshLocalServer(): Promise<ManagedServerStatus | null> {
    if (!api.managedServerStatus) return Promise.resolve(null);
    const current = reads.begin();
    return Promise.resolve(api.managedServerStatus()).then(
      (status) => {
        if (!status) return null;
        if (current()) localServer.read(store, status);
        return status;
      },
      (error: unknown) => {
        if (current()) localServer.readFailed(store, String(error));
        return null;
      },
    );
  }

  /* a change the client reports may be the service's: read it again while
     some screen shows it */
  client.subscribe((state, prev) => {
    if (state.snapshot !== prev.snapshot && watchers > 0) void refreshLocalServer();
  });

  function operate<T>(
    operation: LocalServerOperation,
    run: () => Promise<T>,
    key: string | symbol = operation,
  ): Promise<T> {
    if (running?.key === key) return running.done as Promise<T>;
    const before = running?.done ?? Promise.resolve();
    const done: Promise<T> = before
      .catch(() => {})
      .then(async () => {
        localServer.operating(store, operation);
        reads.supersede();
        waits.supersede();
        publishWait(null);
        try {
          return await run();
        } finally {
          localServer.operating(store, null);
        }
      });
    running = { key, done };
    void done
      .catch(() => {})
      .finally(() => {
        if (running?.done === done) running = null;
      });
    return done;
  }

  const unavailable = (what: string) =>
    Promise.reject(new Error(`${what} is unavailable in this build.`));

  return {
    /** Which local-server commands this build's bridge offers. */
    localServerOffers: {
      get status() {
        return Boolean(api.managedServerStatus);
      },
      get start() {
        return Boolean(api.startManagedServer);
      },
      get stop() {
        return Boolean(api.stopManagedServer);
      },
      get restart() {
        return Boolean(api.restartManagedServer);
      },
      get autostart() {
        return Boolean(api.setManagedServerAutoStart);
      },
      get loginItems() {
        return Boolean(api.openManagedServerLoginItems);
      },
    },
    /** Reads the service's status into the store; resolves with it, or with
        null when it could not be read (the store holds why). */
    refreshLocalServer,
    /** Keeps the status read while a screen shows it: now, and again each
        time the client reports a change. Returns the end of the watch. */
    watchLocalServer() {
      watchers += 1;
      void refreshLocalServer();
      return () => {
        watchers -= 1;
      };
    },
    /** Starts the service, publishing which wait it is in while it starts. */
    startLocalServer(nodeName: string, authority?: ManagedServerAuthorityInput) {
      const start = api.startManagedServer;
      if (!start) return unavailable("Start Node");
      return operate(
        "start",
        () =>
          observeManagedServerOperation(
            api,
            () => start(nodeName, authority),
            publishWait,
          ),
        JSON.stringify([
          "start",
          nodeName,
          authority?.toolCeiling,
          authority?.toolRoot,
        ]),
      ).then(settled);
    },
    /** Stops the service; it stays enabled at login. */
    stopLocalServer() {
      const stop = api.stopManagedServer;
      if (!stop) return unavailable("Stop Node");
      return operate("stop", () => stop(false)).then(settled);
    },
    /** Restarts the service with `authority`, publishing its wait. */
    restartLocalServer(nodeName: string, authority: ManagedServerAuthorityInput) {
      const restart = api.restartManagedServer;
      if (!restart) return unavailable("Restart Node");
      return operate(
        "restart",
        () =>
          observeManagedServerOperation(
            api,
            () => restart(nodeName, authority),
            publishWait,
          ),
        JSON.stringify([
          "restart",
          nodeName,
          authority.toolCeiling,
          authority.toolRoot,
        ]),
      ).then(settled);
    },
    /** Turns starting the service at login on or off. */
    setLocalServerAutoStart(enabled: boolean) {
      const set = api.setManagedServerAutoStart;
      if (!set) return unavailable("Start at login");
      return operate("autostart", () => set(enabled), `autostart:${enabled}`).then(
        settled,
      );
    },
    /** Commits a new node to starting at login, as first run does. */
    commitLocalServerAutoStart(nodeName: string) {
      const commit = api.commitManagedServerAutoStart;
      if (!commit) return Promise.resolve(store.getState().status);
      return operate(
        "autostart",
        () => commit(nodeName),
        `commit-autostart:${nodeName}`,
      ).then(settled);
    },
    /** Waits until the service reports secure background pairing ready. */
    awaitLocalServerPairing(
      options?: Parameters<typeof waitForManagedRuntimePairing>[1],
    ) {
      const current = reads.begin();
      return waitForManagedRuntimePairing(api, options).then((status) => {
        if (status && current()) localServer.read(store, status);
        return status;
      });
    },
    /** Makes sure the service is serving before setup writes to it: waits
        out a boot or an approval, starts a stopped service, then waits for
        pairing. */
    ensureLocalServerServing(
      fallbackAgentName: string,
      options: Omit<Parameters<typeof ensureManagedRuntimeServing>[2], "onWait"> = {},
    ) {
      return operate(
        "ensure",
        () =>
          ensureManagedRuntimeServing(api, fallbackAgentName, {
            ...options,
            onWait: publishWait,
          }),
        Symbol("ensure"),
      ).finally(() => void refreshLocalServer());
    },
    /** Waits while the service boots, updates its data or awaits macOS
        approval, within the bridge's bounds, publishing which wait it is
        in; resolves with the last status. */
    settleLocalServer(status: ManagedServerStatus) {
      const current = running ? () => false : reads.begin();
      const currentWait = running ? () => false : waits.begin();
      return awaitManagedServerSettled(api, status, (wait) => {
        if (currentWait() && !running) publishWait(wait);
      }).then((next) => {
        if (current() && !running) localServer.read(store, next);
        return next;
      });
    },
    /** Observes the service at launch (see `restoreManagedServer`),
        publishing which wait it is in. */
    restoreLocalServer(signal?: AbortSignal) {
      if (signal?.aborted) return Promise.resolve(false);
      const pending = operate(
        "restore",
        () =>
          signal?.aborted
            ? Promise.resolve(false)
            : restoreManagedServer(api, { onWait: publishWait, signal }),
        Symbol("restore"),
      );
      if (!signal) return pending;
      return new Promise<boolean | null>((resolve, reject) => {
        const abort = () => resolve(false);
        signal.addEventListener("abort", abort, { once: true });
        pending.then(
          (result) => {
            signal.removeEventListener("abort", abort);
            resolve(result);
          },
          (error: unknown) => {
            signal.removeEventListener("abort", abort);
            reject(error);
          },
        );
      });
    },
    /** Resolves a tool root as the service will use it, or rejects saying why. */
    validateLocalServerRoot(path: string) {
      const validate = api.validateManagedServerRoot;
      return validate ? validate(path) : Promise.resolve(path);
    },
    /** Opens the system settings where macOS allows Gents in the background. */
    openLocalServerLoginItems() {
      return api.openManagedServerLoginItems?.() ?? Promise.resolve();
    },
  };
}

export type LocalServerActions = ReturnType<typeof createLocalServer>;
