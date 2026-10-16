import { useMemo } from "react";
import { useStore } from "zustand";

import {
  BridgeInvokeError,
  type DesktopApiAdapter,
  type ManagedServerResetResult,
} from "@source-inc/gents-desktop-client";

import { ManagedServerStartupError } from "../lib/managedServerStartup";
import type { ClientState, ClientStore } from "./clientStore";
import { errorText } from "../lib/errorText";

type IncompatibleHomeOptions = {
  api: DesktopApiAdapter;
  client: ClientStore;
  setError: (error: string | null) => void;
  startFresh: () => Promise<void>;
};

export type IncompatibleHome = {
  report: ManagedServerResetResult | null;
  busy: boolean;
  fullReset: boolean;
  canReset: boolean;
  previewReset: () => Promise<void>;
  cancel: () => void;
  generation: number;
  adopt: (error: unknown) => Promise<boolean>;
  backUp: () => Promise<void>;
  remove: () => Promise<void>;
  keep: () => Promise<void>;
  continueFresh: () => Promise<void>;
};

export function isIncompatibleHomeError(error: unknown): boolean {
  if (error instanceof BridgeInvokeError)
    return error.code === "incompatibleLocalStore";
  if (error instanceof ManagedServerStartupError) {
    return error.status.errorCode === "incompatibleLocalStore";
  }
  return false;
}

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
        setHome({ report: await resetManagedServer(), fullReset: false });
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

  async function previewReset() {
    if (!api.resetManagedServer || home().busy) return;
    setBusy(true);
    setError(null);
    try {
      const report = await api.resetManagedServer(undefined, undefined, true);
      setHome({ report, fullReset: true });
    } catch (error) {
      setError(errorText(error));
    } finally {
      setBusy(false);
    }
  }

  function cancel() {
    if (home().busy || home().report?.completed) return;
    setHome({ report: null, fullReset: false });
    setError(null);
  }

  async function retire(disposition: "archive" | "delete") {
    const report = home().report;
    if (!report || report.completed || home().busy || !api.resetManagedServer) return;
    const confirmation =
      disposition === "archive" ? report.confirmation : report.deleteConfirmation;
    if (!confirmation) return;
    setBusy(true);
    setError(null);
    let handedOff = false;
    try {
      const result = home().fullReset
        ? await api.resetManagedServer(confirmation, disposition, true)
        : await api.resetManagedServer(confirmation, disposition);
      if (home().fullReset && !result.completed) {
        handedOff = true;
        return;
      }
      if (!result.completed) throw new Error("the home was not reset");
      if (disposition === "archive" && !result.backupPath) {
        throw new Error("the home was not backed up");
      }
      setReport(result);
    } catch (error) {
      setError(errorText(error));
      if (error instanceof BridgeInvokeError && error.code === "invalidArgument") {
        try {
          setReport(
            home().fullReset
              ? await api.resetManagedServer(undefined, undefined, true)
              : await api.resetManagedServer(),
          );
        } catch {
        }
      }
    } finally {
      if (!handedOff) setBusy(false);
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
    canReset: Boolean(api.resetManagedServer),
    previewReset,
    cancel,
    backUp: () => retire("archive"),
    remove: () => retire("delete"),
    keep,
    continueFresh,
  };
}

export function useIncompatibleHome(
  client: ClientStore,
  ops: ReturnType<typeof createIncompatibleHomeOps>,
): IncompatibleHome {
  const state = useStore(client, (s) => s.home);
  return useMemo(() => ({ ...state, ...ops }), [state, ops]);
}
