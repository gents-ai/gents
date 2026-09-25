import { useRef, useState } from "react";

import {
  BridgeInvokeError,
  type DesktopApiAdapter,
  type ManagedServerResetResult,
} from "@source-inc/gents-desktop-client";

import { ManagedServerStartupError } from "../lib/managedServerStartup";

type IncompatibleHomeOptions = {
  api: DesktopApiAdapter;
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
export function useIncompatibleHome({
  api,
  setError,
  startFresh,
}: IncompatibleHomeOptions): IncompatibleHome {
  const [report, setReport] = useState<ManagedServerResetResult | null>(null);
  const [busy, setBusy] = useState(false);
  const [generation, setGeneration] = useState(0);
  const previewing = useRef<Promise<boolean> | null>(null);

  function adopt(error: unknown): Promise<boolean> {
    if (!isIncompatibleHomeError(error) || !api.resetManagedServer) {
      return Promise.resolve(false);
    }
    if (previewing.current) return previewing.current;
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
        previewing.current = null;
      }
    })();
    previewing.current = pending;
    return pending;
  }

  async function retire(disposition: "archive" | "delete") {
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
    setGeneration((current) => current + 1);
    await startFresh();
  }

  return {
    report,
    busy,
    generation,
    adopt,
    backUp: () => retire("archive"),
    remove: () => retire("delete"),
    keep,
    continueFresh,
  };
}
