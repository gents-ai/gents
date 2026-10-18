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

export function createLocalServer({ api, store, client }: LocalServerParams) {
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
    refreshLocalServer,
    watchLocalServer() {
      watchers += 1;
      void refreshLocalServer();
      return () => {
        watchers -= 1;
      };
    },
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
    stopLocalServer() {
      const stop = api.stopManagedServer;
      if (!stop) return unavailable("Stop Node");
      return operate("stop", () => stop(false)).then(settled);
    },
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
    setLocalServerAutoStart(enabled: boolean) {
      const set = api.setManagedServerAutoStart;
      if (!set) return unavailable("Start at login");
      return operate("autostart", () => set(enabled), `autostart:${enabled}`).then(
        settled,
      );
    },
    commitLocalServerAutoStart(nodeName: string) {
      const commit = api.commitManagedServerAutoStart;
      if (!commit) return Promise.resolve(store.getState().status);
      return operate(
        "autostart",
        () => commit(nodeName),
        `commit-autostart:${nodeName}`,
      ).then(settled);
    },
    awaitLocalServerPairing(
      options?: Parameters<typeof waitForManagedRuntimePairing>[1],
    ) {
      const current = reads.begin();
      return waitForManagedRuntimePairing(api, options).then((status) => {
        if (status && current()) localServer.read(store, status);
        return status;
      });
    },
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
    validateLocalServerRoot(path: string) {
      const validate = api.validateManagedServerRoot;
      return validate ? validate(path) : Promise.resolve(path);
    },
    openLocalServerLoginItems() {
      return api.openManagedServerLoginItems?.() ?? Promise.resolve();
    },
  };
}

export type LocalServerActions = ReturnType<typeof createLocalServer>;
