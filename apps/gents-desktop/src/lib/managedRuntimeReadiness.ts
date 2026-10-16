import type {
  DesktopApiAdapter,
  ManagedServerStatus,
} from "@source-inc/gents-desktop-client";
import { DEFAULT_NODE_NAME } from "@source-inc/gents-desktop-fleet";

import {
  awaitManagedServerSettled,
  ManagedServerStartupError,
  observeManagedServerOperation,
  unsettledManagedServerError,
  type ManagedServerWait,
} from "./managedServerStartup";
import { setupErrorMessage } from "./setupErrors";

type ReadinessApi = Pick<
  DesktopApiAdapter,
  "managedServerStatus" | "startManagedServer"
>;

export const MANAGED_RUNTIME_READY_TIMEOUT_MS = 30_000;

const RUNTIME_UNAVAILABLE =
  "The local node is not running, so provider sign-in is unavailable. Start the node and try again.";

export class ManagedRuntimeUnavailableError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ManagedRuntimeUnavailableError";
  }
}

const sleep = (ms: number) =>
  new Promise<void>((resolve) => globalThis.setTimeout(resolve, ms));

export async function waitForManagedRuntimePairing(
  api: ReadinessApi,
  { timeoutMs = MANAGED_RUNTIME_READY_TIMEOUT_MS, intervalMs = 250 } = {},
): Promise<ManagedServerStatus | null> {
  if (!api.managedServerStatus) return null;
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const status = await api.managedServerStatus();
    if (status.pairingReady) return status;
    await sleep(intervalMs);
  }
  const status = await api.managedServerStatus();
  if (!status.pairingReady) {
    throw new ManagedRuntimeUnavailableError(
      "The local node started, but secure background pairing is not ready.",
    );
  }
  return status;
}

export async function ensureManagedRuntimeServing(
  api: ReadinessApi,
  fallbackNodeName: string,
  options: {
    timeoutMs?: number;
    intervalMs?: number;
    onWait?: (wait: ManagedServerWait | null) => void;
    settle?: Parameters<typeof awaitManagedServerSettled>[3];
  } = {},
): Promise<void> {
  if (!api.managedServerStatus) return;
  const { onWait = () => {}, settle, ...pairing } = options;
  let status: ManagedServerStatus;
  try {
    status = await api.managedServerStatus();
    if (status.pairingReady) return;
    status = await awaitManagedServerSettled(api, status, onWait, settle);
    const unsettled = unsettledManagedServerError(status);
    if (unsettled && unsettled.status.state !== "failed") throw unsettled;
    const needsStart =
      status.state === "stopped" ||
      status.state === "failed" ||
      status.state === "disabled";
    if (needsStart) {
      const startManagedServer = api.startManagedServer;
      if (!startManagedServer) {
        throw new ManagedRuntimeUnavailableError("The local node is not running.");
      }
      const nodeName =
        status.nodeName?.trim() || fallbackNodeName.trim() || DEFAULT_NODE_NAME;
      try {
        await observeManagedServerOperation(
          api,
          () => startManagedServer(nodeName),
          onWait,
        );
      } catch (cause) {
        throw new ManagedRuntimeUnavailableError(
          setupErrorMessage(cause, RUNTIME_UNAVAILABLE),
        );
      }
    }
    await waitForManagedRuntimePairing(api, pairing);
  } catch (cause) {
    console.warn("managed runtime is not serving for setup", cause);
    throw new ManagedRuntimeUnavailableError(
      cause instanceof ManagedServerStartupError ||
        cause instanceof ManagedRuntimeUnavailableError
        ? cause.message
        : RUNTIME_UNAVAILABLE,
    );
  }
}
