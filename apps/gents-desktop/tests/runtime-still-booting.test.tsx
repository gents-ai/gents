import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  BridgeInvokeError,
  type DesktopApiAdapter,
  type ManagedServerStatus,
} from "@source-inc/gents-desktop-client";

const toast = vi.hoisted(() =>
  Object.assign(vi.fn(), {
    error: vi.fn(),
    loading: vi.fn(),
    dismiss: vi.fn(),
  }),
);
vi.mock("sonner", () => ({ toast }));

import {
  installManagedServerTrayListeners,
  MANAGED_SERVER_TRAY_RESTART_EVENT,
  MANAGED_SERVER_TRAY_START_EVENT,
} from "../src/lib/managedServerTray";
import { LocalServer } from "../src/ui/screens/agent/LocalServer";
import type { Shell } from "../src/ui/hooks/useShell";

const base: ManagedServerStatus = {
  state: "running",
  autoStart: true,
  agentName: "Workshop Agent",
  agentDid: "did:key:agent",
  graphql: "http://127.0.0.1:9191/graphql",
  effectiveToolCeiling: "readwrite",
  effectiveToolRoot: "/Users/test",
  suggestedToolRoot: "/Users/test",
  pairingReady: true,
  approvalRequired: false,
  runtimeBooting: false,
  error: null,
};
const stopped: ManagedServerStatus = { ...base, state: "stopped", pairingReady: false };
const updating: ManagedServerStatus = {
  ...base,
  state: "starting",
  runtimeBooting: true,
  pairingReady: false,
};
const stillBooting = () =>
  new BridgeInvokeError({
    code: "runtimeStillBooting",
    message: "The runtime is still updating its data.",
    retryable: true,
    endpoint: null,
  });

/** The bridge reports the runtime still migrating, then running. */
function statusSequence(...statuses: ManagedServerStatus[]) {
  let index = 0;
  return vi.fn(async () => statuses[Math.min(index++, statuses.length - 1)]);
}

beforeEach(() => {
  vi.clearAllMocks();
});

describe("runtimeStillBooting is a wait, not a failure", () => {
  it("tray Start observes a still-booting runtime until it is ready", async () => {
    const handlers = new Map<string, () => void>();
    const reportError = vi.fn();
    const onWait = vi.fn();
    const api = {
      managedServerStatus: statusSequence(stopped, updating, base),
      startManagedServer: vi.fn(async () => {
        throw stillBooting();
      }),
    } as unknown as DesktopApiAdapter;
    installManagedServerTrayListeners(
      api,
      async (event, handler) => {
        handlers.set(event, handler);
        return () => {};
      },
      reportError,
      async () => {},
      onWait,
    );
    await Promise.resolve();
    handlers.get(MANAGED_SERVER_TRAY_START_EVENT)?.();
    await vi.waitFor(() => expect(onWait).toHaveBeenLastCalledWith(null), {
      timeout: 5_000,
    });
    expect(onWait).toHaveBeenCalledWith(expect.objectContaining({ kind: "updating" }));
    expect(api.startManagedServer).toHaveBeenCalledOnce();
    expect(reportError).not.toHaveBeenCalled();
  });

  it("tray Restart waits for an updating runtime instead of restarting it", async () => {
    const handlers = new Map<string, () => void>();
    const reportError = vi.fn();
    const onWait = vi.fn();
    const api = {
      managedServerStatus: statusSequence(updating, base),
      restartManagedServer: vi.fn(),
    } as unknown as DesktopApiAdapter;
    installManagedServerTrayListeners(
      api,
      async (event, handler) => {
        handlers.set(event, handler);
        return () => {};
      },
      reportError,
      async () => {},
      onWait,
    );
    await Promise.resolve();
    handlers.get(MANAGED_SERVER_TRAY_RESTART_EVENT)?.();
    await vi.waitFor(() => expect(onWait).toHaveBeenLastCalledWith(null), {
      timeout: 5_000,
    });
    expect(onWait).toHaveBeenCalledWith(expect.objectContaining({ kind: "updating" }));
    expect(api.restartManagedServer).not.toHaveBeenCalled();
    expect(reportError).not.toHaveBeenCalled();
  });

  it("tray Restart observes a still-booting restart result", async () => {
    const handlers = new Map<string, () => void>();
    const reportError = vi.fn();
    const api = {
      managedServerStatus: statusSequence(base, updating, base),
      restartManagedServer: vi.fn(async () => {
        throw stillBooting();
      }),
    } as unknown as DesktopApiAdapter;
    const onWait = vi.fn();
    installManagedServerTrayListeners(
      api,
      async (event, handler) => {
        handlers.set(event, handler);
        return () => {};
      },
      reportError,
      async () => {},
      onWait,
    );
    await Promise.resolve();
    handlers.get(MANAGED_SERVER_TRAY_RESTART_EVENT)?.();
    await vi.waitFor(() => expect(onWait).toHaveBeenLastCalledWith(null), {
      timeout: 5_000,
    });
    expect(api.restartManagedServer).toHaveBeenCalledOnce();
    expect(reportError).not.toHaveBeenCalled();
  });

  it("LocalServer Start shows Updating data… and reports success, not a failure", async () => {
    const user = userEvent.setup();
    const api = {
      managedServerStatus: statusSequence(stopped, updating, updating, base),
      startManagedServer: vi.fn(async () => {
        throw stillBooting();
      }),
      stopManagedServer: vi.fn(),
    } as unknown as DesktopApiAdapter;
    const shell = { api, snapshot: {}, refreshSnapshot: vi.fn() } as unknown as Shell;
    render(<LocalServer shell={shell} />);
    await user.click(await screen.findByRole("button", { name: "Start agent" }));
    expect(await screen.findByText("Updating data…")).toBeInTheDocument();
    await vi.waitFor(() => expect(toast).toHaveBeenCalledWith("Agent started"), {
      timeout: 5_000,
    });
    expect(toast).not.toHaveBeenCalledWith(expect.stringContaining("failed"));
  });

  it("LocalServer never offers Change access (a restart) while the runtime updates", async () => {
    const api = {
      managedServerStatus: vi.fn(async () => updating),
      startManagedServer: vi.fn(),
    } as unknown as DesktopApiAdapter;
    const shell = { api, snapshot: {}, refreshSnapshot: vi.fn() } as unknown as Shell;
    render(<LocalServer shell={shell} />);
    expect(await screen.findByText("updating data")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Change access…" })).toBeNull();
  });
});
