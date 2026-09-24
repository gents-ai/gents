import type {
  DesktopApiAdapter,
  ManagedServerStatus,
} from "@source-inc/gents-desktop-client";

export const MANAGED_SERVER_BOOT_TIMEOUT_MS = 5 * 60_000;
export const MANAGED_SERVER_APPROVAL_TIMEOUT_MS = 10 * 60_000;
export const MANAGED_SERVER_POLL_INTERVAL_MS = 1_000;

export type ManagedServerWaitKind = "approval" | "booting";

export type ManagedServerWait = {
  kind: ManagedServerWaitKind;
  since: number;
};

export function managedServerWaitKind(
  status: ManagedServerStatus,
): ManagedServerWaitKind | null {
  if (status.approvalRequired) return "approval";
  if (status.state === "starting") return "booting";
  return null;
}

export function nextManagedServerWait(
  previous: ManagedServerWait | null,
  status: ManagedServerStatus,
  now: number,
): ManagedServerWait | null {
  const kind = managedServerWaitKind(status);
  if (!kind) return null;
  return previous?.kind === kind ? previous : { kind, since: now };
}

export function formatElapsed(ms: number): string {
  const seconds = Math.max(0, Math.floor(ms / 1000));
  if (seconds < 60) return `${seconds}s`;
  return `${Math.floor(seconds / 60)}m ${String(seconds % 60).padStart(2, "0")}s`;
}

export const LOGIN_ITEMS_PATH = "System Settings › General › Login Items & Extensions";

export function describeManagedServerWait(
  wait: ManagedServerWait,
  now: number,
): { label: string; detail: string } {
  const elapsed = formatElapsed(now - wait.since);
  if (wait.kind === "approval") {
    return {
      label: "Waiting for macOS to allow Gents in the background",
      detail: `Gents runs its agent as a background item. Turn on Gents under ${LOGIN_ITEMS_PATH} › Allow in the Background. Startup continues by itself once it is allowed. Waiting ${elapsed}.`,
    };
  }
  return {
    label: "Waiting for the background agent to finish starting",
    detail: `Gents is starting the background agent. It has not reported ready yet. Waiting ${elapsed}.`,
  };
}

const delay = (ms: number) =>
  new Promise<void>((resolve) => globalThis.setTimeout(resolve, ms));

/**
 * Runs a managed-server operation while republishing the bridge's status
 * observation, so a long start shows which wait it is in.
 */
export async function observeManagedServerOperation<T>(
  api: Pick<DesktopApiAdapter, "managedServerStatus">,
  operation: () => Promise<T>,
  onWait: (wait: ManagedServerWait | null) => void,
  intervalMs = MANAGED_SERVER_POLL_INTERVAL_MS,
): Promise<T> {
  let settled = false;
  let wait: ManagedServerWait | null = null;
  const pending = operation();
  const done = pending.then(
    () => undefined,
    () => undefined,
  );
  void done.then(() => {
    settled = true;
  });
  void (async () => {
    while (!settled && api.managedServerStatus) {
      await Promise.race([delay(intervalMs), done]);
      if (settled) return;
      try {
        const status = await api.managedServerStatus();
        if (settled) return;
        wait = nextManagedServerWait(wait, status, Date.now());
        onWait(wait);
      } catch {
        /* The operation's own result reports failures. */
      }
    }
  })();
  try {
    return await pending;
  } finally {
    settled = true;
    onWait(null);
  }
}

/** The background agent did not settle: it timed out waiting or reported a failure. */
export class ManagedServerStartupError extends Error {
  constructor(
    message: string,
    readonly status: ManagedServerStatus,
  ) {
    super(message);
    this.name = "ManagedServerStartupError";
  }
}

export function unsettledManagedServerError(
  status: ManagedServerStatus,
): ManagedServerStartupError | null {
  const kind = managedServerWaitKind(status);
  if (kind === "approval") {
    return new ManagedServerStartupError(
      `Gents is still not allowed to run in the background. Turn on Gents under ${LOGIN_ITEMS_PATH}, then try again.`,
      status,
    );
  }
  if (kind === "booting") {
    return new ManagedServerStartupError(
      `The background agent did not report ready within ${MANAGED_SERVER_BOOT_TIMEOUT_MS / 60_000} minutes. Restart the agent, or try again.`,
      status,
    );
  }
  if (status.state === "failed" && status.error) {
    return new ManagedServerStartupError(status.error, status);
  }
  return null;
}

/**
 * Waits while the bridge observes the managed service booting or awaiting
 * macOS approval, within the same bounds the bridge applies to its own waits.
 * Resolves with the last observation; `signal` ends the wait early when the
 * user chooses to continue without it.
 */
export async function awaitManagedServerSettled(
  api: Pick<DesktopApiAdapter, "managedServerStatus">,
  initial: ManagedServerStatus,
  onWait: (wait: ManagedServerWait | null) => void,
  {
    timeoutsMs = {
      booting: MANAGED_SERVER_BOOT_TIMEOUT_MS,
      approval: MANAGED_SERVER_APPROVAL_TIMEOUT_MS,
    },
    intervalMs = MANAGED_SERVER_POLL_INTERVAL_MS,
    signal,
  }: {
    timeoutsMs?: Record<ManagedServerWaitKind, number>;
    intervalMs?: number;
    signal?: AbortSignal;
  } = {},
): Promise<ManagedServerStatus> {
  let status = initial;
  let wait: ManagedServerWait | null = null;
  const aborted = new Promise<void>((resolve) => {
    if (signal?.aborted) resolve();
    signal?.addEventListener("abort", () => resolve(), { once: true });
  });
  try {
    while (api.managedServerStatus && !signal?.aborted) {
      wait = nextManagedServerWait(wait, status, Date.now());
      if (!wait || Date.now() - wait.since >= timeoutsMs[wait.kind]) break;
      onWait(wait);
      await Promise.race([delay(intervalMs), aborted]);
      if (signal?.aborted) break;
      status = await api.managedServerStatus();
    }
    return status;
  } finally {
    onWait(null);
  }
}
