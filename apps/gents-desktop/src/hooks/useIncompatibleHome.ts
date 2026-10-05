import { useMemo } from "react";
import { useStore } from "zustand";

import {
  BridgeInvokeError,
  type DesktopApiAdapter,
  type ManagedServerResetResult,
} from "@source-inc/gents-desktop-client";

import { ManagedServerStartupError } from "../lib/managedServerStartup";
import type { ClientState, ClientStore } from "./clientStore";

type IncompatibleHomeOptions = {
  api: DesktopApiAdapter;
  /** where the report lives, read by an action when it runs */
  client: ClientStore;
  setError: (error: string | null) => void;
  /** Runs startup again once the old home is out of the way. */
  startFresh: () => Promise<void>;
};

export type IncompatibleHome = {
  /** The bridge's account of the home: a preview until a reset completes. */
  report: ManagedServerResetResult | null;
  busy: boolean;
  /** Increments each time the user continues from a completed reset. */
  generation: number;
  /**
   * Takes the error of a failed startup, setup start, restart, or client
   * start. Resolves true when the bridge classified it as a store this
   * version cannot open and the home was previewed for a reset.
   */
  adopt: (error: unknown) => Promise<boolean>;
  backUp: () => Promise<void>;
  remove: () => Promise<void>;
  keep: () => Promise<void>;
  continueFresh: () => Promise<void>;
};

/** The bridge's typed classification; never inferred from message text. */
export function isIncompatibleHomeError(error: unknown): boolean {
  if (error instanceof BridgeInvokeError)
    return error.code === "incompatibleLocalStore";
  if (error instanceof ManagedServerStartupError) {
    return error.status.errorCode === "incompatibleLocalStore";
  }
  return false;
}

/** Owns the one decision flow for a local home this version cannot open. */
/** What the person can do about a home this version cannot open. Made
    once; each reads the report from the client store when it runs. */
export function createIncompatibleHomeOps({
  api,
  client,
  setError,
  startFresh,
}: IncompatibleHomeOptions) {
  let previewing: Promise<boolean> | null = null;
  const home = () => client.getState().home;
  const setHome = (patch: Partial<ClientState["home"]>) =>
    client.setState((state) => ({ home: { ...state.home, ...patch } }));
  const setReport = (next: ManagedServerResetResult | null) =>
    setHome({ report: next });
  const setBusy = (next: boolean) => setHome({ busy: next });

  function adopt(error: unknown): Promise<boolean> {
    if (!isIncompatibleHomeError(error) || !api.resetManagedServer) {
      return Promise.resolve(false);
    }
    if (previewing) return previewing;
    const resetManagedServer = api.resetManagedServer;
    const pending = (async () => {
      try {
        setReport(await resetManagedServer());
        return true;
      } catch (previewError) {
        setError(
          `${String(error)} Inspecting the home failed: ${String(previewError)}`,
        );
        return false;
      } finally {
        previewing = null;
      }
    })();
    previewing = pending;
    return pending;
  }

  async function retire(disposition: "archive" | "delete") {
    const report = home().report;
    if (!report || report.completed || !api.resetManagedServer) return;
    const confirmation =
      disposition === "archive" ? report.confirmation : report.deleteConfirmation;
    if (!confirmation) return;
    setBusy(true);
    setError(null);
    try {
      const result = await api.resetManagedServer(confirmation, disposition);
      if (!result.completed) throw new Error("the home was not reset");
      if (disposition === "archive" && !result.backupPath) {
        throw new Error("the home was not backed up");
      }
      setReport(result);
    } catch (error) {
      setError(error instanceof Error ? error.message : String(error));
      // The bridge refuses a confirmation whose home changed since review;
      // show the current plan so the user reviews what would change now.
      if (error instanceof BridgeInvokeError && error.code === "invalidArgument") {
        try {
          setReport(await api.resetManagedServer());
        } catch {
          /* The refusal above already explains the failure. */
        }
      }
    } finally {
      setBusy(false);
    }
  }

  async function keep() {
    setBusy(true);
    try {
      if (api.quitDesktop) await api.quitDesktop();
      else window.close();
    } finally {
      setBusy(false);
    }
  }

  async function continueFresh() {
    setReport(null);
    setError(null);
    setHome({ generation: home().generation + 1 });
    await startFresh();
  }

  return {
    adopt,
    backUp: () => retire("archive"),
    remove: () => retire("delete"),
    keep,
    continueFresh,
  };
}

/** The incompatible home as screens read it: its state and its actions. */
export function useIncompatibleHome(
  client: ClientStore,
  ops: ReturnType<typeof createIncompatibleHomeOps>,
): IncompatibleHome {
  const state = useStore(client, (s) => s.home);
  return useMemo(() => ({ ...state, ...ops }), [state, ops]);
}
