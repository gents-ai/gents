import { act, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { renderIn, testApp } from "./app-fixture";

import type {
  DesktopApiAdapter,
  ManagedServerAuthorityInput,
  ManagedServerStatus,
} from "@source-inc/gents-desktop-client";
import type { ManagedServerRepairResult } from "@source-inc/gents-desktop-client/generated/ManagedServerRepairResult";

import { StartupScreen } from "../src/components/StartupScreen";
import { restoreManagedServer } from "../src/hooks/managedServerLifecycle";
import { BridgeInvokeError } from "@source-inc/gents-desktop-client";
import {
  awaitManagedServerSettled,
  MANAGED_SERVER_BOOT_TIMEOUT_MS,
  ManagedServerStartupError,
  managedServerWaitKind,
  nextManagedServerWait,
  observeManagedServerOperation,
  unsettledManagedServerError,
  type ManagedServerWait,
} from "../src/lib/managedServerStartup";
import { SetupScreen } from "../src/ui/screens/setup/SetupScreen";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";

function managedStatus(
  overrides: Partial<ManagedServerStatus> = {},
): ManagedServerStatus {
  return {
    state: "disabled",
    autoStart: false,
    nodeName: null,
    nodeDid: null,
    graphql: null,
    effectiveToolCeiling: null,
    effectiveToolRoot: null,
    suggestedToolRoot: "/Users/test",
    pairingReady: false,
    approvalRequired: false,
    runtimeBooting: false,
    error: null,
    ...overrides,
  };
}

describe("managed server startup waits", () => {
  it("waits out a data update longer than the boot bound at launch", async () => {
    vi.useFakeTimers();
    try {
      const updatingUntil = Date.now() + MANAGED_SERVER_BOOT_TIMEOUT_MS + 60_000;
      const api = {
        managedServerStatus: vi.fn(async () =>
          Date.now() < updatingUntil
            ? managedStatus({ state: "starting", runtimeBooting: true })
            : managedStatus({ state: "running", nodeDid: "did:key:migrated" }),
        ),
      } as unknown as DesktopApiAdapter;
      const waits: (ManagedServerWait | null)[] = [];
      const restored = restoreManagedServer(api, {
        onWait: (wait) => waits.push(wait),
      });
      await vi.advanceTimersByTimeAsync(MANAGED_SERVER_BOOT_TIMEOUT_MS + 2 * 60_000);
      await expect(restored).resolves.toBe(true);
      expect(waits.some((wait) => wait?.kind === "updating")).toBe(true);
      expect(
        unsettledManagedServerError(
          managedStatus({ state: "starting", runtimeBooting: true }),
        ),
      ).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps observing after an explicit command reports the runtime still booting", async () => {
    let calls = 0;
    const api = {
      managedServerStatus: vi.fn(async () =>
        ++calls < 3
          ? managedStatus({ state: "starting", runtimeBooting: true })
          : managedStatus({ state: "running", nodeDid: "did:key:migrated" }),
      ),
    } as unknown as DesktopApiAdapter;
    const restart = vi.fn(async (): Promise<ManagedServerStatus> => {
      throw new BridgeInvokeError({
        code: "runtimeStillBooting",
        message: "the Gents runtime is still starting after 300 seconds",
        retryable: false,
        endpoint: null,
      });
    });
    const waits: (ManagedServerWait | null)[] = [];
    const status = await observeManagedServerOperation(
      api,
      restart,
      (wait) => waits.push(wait),
      1,
    );
    expect(status.state).toBe("running");
    expect(restart).toHaveBeenCalledOnce();
    expect(waits.some((wait) => wait?.kind === "updating")).toBe(true);

    const failing = vi.fn(async (): Promise<ManagedServerStatus> => {
      throw new BridgeInvokeError({
        code: "backend",
        message: "launchctl failed",
        retryable: true,
        endpoint: null,
      });
    });
    await expect(
      observeManagedServerOperation(api, failing, () => {}, 1),
    ).rejects.toThrow("launchctl failed");
  });

  it("counts an interrupted update toward the booting bound", () => {
    const updating = nextManagedServerWait(
      null,
      managedStatus({ state: "starting", runtimeBooting: true }),
      1_000,
    );
    const booting = nextManagedServerWait(
      updating,
      managedStatus({ state: "starting" }),
      9_000,
    );
    expect(booting).toEqual({ kind: "booting", since: 1_000 });
    const approval = nextManagedServerWait(
      booting,
      managedStatus({ state: "starting", approvalRequired: true }),
      12_000,
    );
    expect(approval).toEqual({ kind: "approval", since: 12_000 });
  });

  it("names a data update without offering a restart", () => {
    const app = testApp({
      api: { restartManagedServer: vi.fn(async () => undefined) },
      supportsManagedServer: true,
    });
    app.stores.client.setState({ startupPhase: "checking-managed-server" });
    app.stores.localServer.setState({
      wait: { kind: "updating", since: Date.now() - 400_000 },
    });
    renderIn(app, <StartupScreen />);
    expect(screen.getByTestId("startup-screen")).toHaveTextContent("Updating data…");
    expect(
      screen.queryByTestId("startup-restart-managed-server"),
    ).not.toBeInTheDocument();
  });

  it("keeps waiting while a slow runtime boots, then reports it running", async () => {
    const statuses = [
      managedStatus({ state: "starting" }),
      managedStatus({ state: "starting" }),
      managedStatus({ state: "running", nodeDid: "did:key:slow" }),
    ];
    const api = {
      managedServerStatus: vi.fn(async () => statuses.shift()!),
    };
    const waits: (ManagedServerWait | null)[] = [];

    const settled = await awaitManagedServerSettled(
      api,
      await api.managedServerStatus(),
      (wait) => waits.push(wait),
      { intervalMs: 1 },
    );

    expect(settled.state).toBe("running");
    expect(waits.filter(Boolean).map((wait) => wait!.kind)).toEqual([
      "booting",
      "booting",
    ]);
    expect(waits.at(-1)).toBeNull();
    expect(waits[0]).toBe(waits[1]);
  });

  it("restores a booting service at launch once it becomes ready", async () => {
    const statuses = [
      managedStatus({ state: "starting" }),
      managedStatus({ state: "running" }),
    ];
    const api = {
      managedServerStatus: vi.fn(async () => statuses.shift()!),
      startManagedServer: vi.fn(),
    } as unknown as DesktopApiAdapter;
    const onWait = vi.fn();

    await expect(restoreManagedServer(api, { onWait })).resolves.toBe(true);
    expect(onWait).toHaveBeenCalledWith(expect.objectContaining({ kind: "booting" }));
    expect(api.startManagedServer).not.toHaveBeenCalled();
  });

  it("stops waiting on a runtime that stays booting past the bridge bound and says so", async () => {
    const api = {
      managedServerStatus: vi.fn(async () => managedStatus({ state: "starting" })),
    };
    const settled = await awaitManagedServerSettled(
      api,
      managedStatus({ state: "starting" }),
      () => {},
      { intervalMs: 1, timeoutsMs: { booting: 20, approval: 20 } },
    );
    const failure = unsettledManagedServerError(settled);
    expect(failure).toBeInstanceOf(ManagedServerStartupError);
    expect(failure!.message).toContain("did not report ready");
  });

  it("surfaces a crash-looping service at launch instead of silently continuing", async () => {
    const api = {
      managedServerStatus: vi.fn(async () =>
        managedStatus({
          state: "failed",
          error:
            "The background node keeps exiting before it becomes ready: it exited with code 78.",
        }),
      ),
    } as unknown as DesktopApiAdapter;
    await expect(restoreManagedServer(api)).rejects.toThrow("exited with code 78");
  });

  it("sets aside a home the node refused as older data, then goes to setup with the note", async () => {
    const note =
      "This version couldn't open the data from an older Gents version, so it was set aside at /Users/test/.gents.from-older-2026-11-12-120000";
    const repairManagedServer = vi
      .fn<() => Promise<ManagedServerRepairResult>>()
      .mockResolvedValueOnce({
        repaired: ["Earlier note."],
        outcome: "ready",
        blocker: null,
        savedPeers: [],
        localNodeLost: false,
      })
      .mockResolvedValueOnce({
        repaired: [note],
        outcome: "needsSetup",
        blocker: null,
        savedPeers: [],
        localNodeLost: false,
      });
    const api = {
      repairManagedServer,
      managedServerStatus: vi.fn(async () =>
        managedStatus({ state: "failed", errorCode: "incompatibleLocalStore" }),
      ),
    } as unknown as DesktopApiAdapter;
    const onRepair = vi.fn();
    await expect(restoreManagedServer(api, { onRepair })).resolves.toBe(false);
    expect(repairManagedServer).toHaveBeenCalledTimes(2);
    expect(onRepair).toHaveBeenLastCalledWith(
      expect.objectContaining({
        outcome: "needsSetup",
        repaired: ["Earlier note.", note],
      }),
    );
  });

  it("keeps the refusal a repair could not set aside, such as another version's data", async () => {
    const repairManagedServer = vi.fn(async () => ({
      repaired: [],
      outcome: "ready" as const,
      blocker: null,
      savedPeers: [],
      localNodeLost: false,
    }));
    const api = {
      repairManagedServer,
      managedServerStatus: vi.fn(async () =>
        managedStatus({
          state: "failed",
          error: "written by a different Gents version",
          errorCode: "incompatibleLocalStore",
        }),
      ),
    } as unknown as DesktopApiAdapter;
    await expect(restoreManagedServer(api)).rejects.toThrow(
      "written by a different Gents version",
    );
    expect(repairManagedServer).toHaveBeenCalledTimes(2);
  });

  const studio = {
    peerId: "studio",
    label: "Studio",
    nodeDid: "did:key:studio",
    addr: "a",
    source: "enrollment",
    graphql: null,
    local: false,
  };
  const pausedApp = (repairPeers: (typeof studio)[]) => {
    const message =
      "The background node keeps exiting before it becomes ready: it exited with code 78.";
    const app = testApp({ supportsManagedServer: true });
    app.stores.client.setState({
      startupPhase: "managed-server-error",
      error: message,
      repairPeers,
    });
    return app;
  };

  it("paused_startup_offers_only_try_again_without_remote_nodes", () => {
    const app = pausedApp([]);
    renderIn(app, <StartupScreen />);
    expect(screen.getByTestId("startup-screen")).toHaveTextContent(
      "Your local node couldn't start",
    );
    expect(screen.getByTestId("startup-retry")).toBeInTheDocument();
    expect(
      screen.queryByTestId("startup-continue-without-managed-server"),
    ).not.toBeInTheDocument();
    expect(screen.queryByText("Restart node")).not.toBeInTheDocument();
  });

  it("paused_startup_offers_continue_with_the_remote_node_name", async () => {
    const app = pausedApp([studio]);
    const skip = vi
      .spyOn(app.lifecycle, "skipManagedServerWait")
      .mockReturnValue(undefined);
    renderIn(app, <StartupScreen />);
    await userEvent.click(screen.getByRole("button", { name: "Continue with Studio" }));
    expect(skip).toHaveBeenCalledOnce();
  });

  it("the_wait_hides_continue_without_remote_nodes", () => {
    const app = testApp({ supportsManagedServer: true });
    app.stores.client.setState({ startupPhase: "checking-managed-server" });
    app.stores.localServer.setState({
      wait: { kind: "approval", since: Date.now() },
    });
    renderIn(app, <StartupScreen />);
    expect(
      screen.queryByTestId("startup-skip-managed-server-wait"),
    ).not.toBeInTheDocument();
  });

  it("names where the logs are when launch startup fails", () => {
    const app = testApp({ supportsManagedServer: true });
    app.stores.client.setState({
      startupPhase: "managed-server-error",
      error:
        "the native Gents service exited normally before it published runtime readiness",
      startupDiagnosticsHint:
        "Console.app: subsystem ai.gents. Runtime startup errors: /desk/logs/runtime-errors.log",
    });
    renderIn(app, <StartupScreen />);
    expect(screen.getByTestId("diagnostics-hint")).toHaveTextContent(
      "/desk/logs/runtime-errors.log",
    );
    act(() =>
      app.stores.client.setState({
        startupPhase: "checking-managed-server",
        error: null,
        startupDiagnosticsHint: "Console.app: subsystem ai.gents",
      }),
    );
    expect(screen.queryByTestId("diagnostics-hint")).not.toBeInTheDocument();
  });

  it("lets launch continue without the local node while approval is pending", async () => {
    const api = {
      managedServerStatus: vi.fn(async () =>
        managedStatus({ state: "stopped", approvalRequired: true }),
      ),
    } as unknown as DesktopApiAdapter;
    const abort = new AbortController();
    const pending = restoreManagedServer(api, {
      onWait: (wait) => {
        if (wait?.kind === "approval") abort.abort();
      },
      signal: abort.signal,
    });
    await expect(pending).resolves.toBe(false);
  });

  it("starts the configured agent with its stored authority once macOS allows it", async () => {
    const statuses = [
      managedStatus({
        state: "stopped",
        approvalRequired: true,
        autoStart: true,
        nodeName: "Workshop Node",
      }),
      managedStatus({ state: "stopped", autoStart: true, nodeName: "Workshop Node" }),
    ];
    const api = {
      managedServerStatus: vi.fn(async () => statuses.shift() ?? managedStatus()),
      startManagedServer: vi.fn(async () =>
        managedStatus({ state: "running", nodeName: "Workshop Node" }),
      ),
    } as unknown as DesktopApiAdapter;

    await expect(restoreManagedServer(api)).resolves.toBe(true);
    expect(api.startManagedServer).toHaveBeenCalledExactlyOnceWith("Workshop Node");
  });

  it("leaves a deliberately stopped agent stopped after macOS allows it", async () => {
    const statuses = [
      managedStatus({
        state: "stopped",
        approvalRequired: true,
        autoStart: false,
        nodeName: "Workshop Node",
      }),
      managedStatus({
        state: "stopped",
        autoStart: false,
        nodeName: "Workshop Node",
      }),
    ];
    const api = {
      managedServerStatus: vi.fn(async () => statuses.shift() ?? managedStatus()),
      startManagedServer: vi.fn(),
    } as unknown as DesktopApiAdapter;

    await expect(restoreManagedServer(api)).resolves.toBe(false);
    expect(api.startManagedServer).not.toHaveBeenCalled();
  });

  it("reports a runtime that keeps dying during migrations instead of waiting on the update", () => {
    const status = managedStatus({
      state: "failed",
      runtimeBooting: true,
      error:
        "The background node keeps exiting before it becomes ready: it exited with code 78 and was restarted 2 times in a row.",
    });
    expect(managedServerWaitKind(status)).toBeNull();
    expect(unsettledManagedServerError(status)?.message).toContain(
      "exited with code 78",
    );
  });

  it("does not wait on approval for a runtime that still serves", () => {
    expect(
      managedServerWaitKind(
        managedStatus({ state: "running", approvalRequired: true }),
      ),
    ).toBeNull();
    expect(
      managedServerWaitKind(
        managedStatus({ state: "starting", approvalRequired: true }),
      ),
    ).toBe("approval");
  });

  it("shows macOS approval guidance with a settings shortcut on the launch screen", async () => {
    const open = vi.fn(async () => undefined);
    const app = testApp({
      api: { openManagedServerLoginItems: open },
      supportsManagedServer: true,
    });
    app.stores.client.setState({
      startupPhase: "checking-managed-server",
      repairPeers: [studio],
    });
    app.stores.localServer.setState({
      wait: { kind: "approval", since: Date.now() - 12_000 },
    });
    const skip = vi
      .spyOn(app.lifecycle, "skipManagedServerWait")
      .mockReturnValue(undefined);
    renderIn(app, <StartupScreen />);

    const screenText = screen.getByTestId("startup-screen");
    expect(screenText).toHaveTextContent(
      "Waiting for macOS to allow Gents in the background",
    );
    expect(screenText).toHaveTextContent("Login Items & Extensions");
    expect(screenText).toHaveTextContent("Waiting 12s");
    await userEvent.click(screen.getByTestId("startup-open-login-items"));
    expect(open).toHaveBeenCalledOnce();
    expect(screen.getByTestId("startup-skip-managed-server-wait")).toHaveTextContent(
      "Continue with Studio",
    );
    await userEvent.click(screen.getByTestId("startup-skip-managed-server-wait"));
    expect(skip).toHaveBeenCalledOnce();
  });

  it("names a booting runtime and how long it has waited instead of failing", () => {
    const app = testApp({ supportsManagedServer: true });
    app.stores.client.setState({ startupPhase: "checking-managed-server" });
    app.stores.localServer.setState({
      wait: { kind: "booting", since: Date.now() - 75_000 },
    });
    renderIn(app, <StartupScreen />);
    const screenText = screen.getByTestId("startup-screen");
    expect(screenText).toHaveTextContent(
      "Waiting for the background node to finish starting",
    );
    expect(screenText).toHaveTextContent("Waiting 1m 15s");
    expect(screen.queryByTestId("startup-retry")).not.toBeInTheDocument();
  });
});

describe("first-run local node startup", () => {
  function firstRun() {
    let observed = managedStatus();
    let resolveStart!: (status: ManagedServerStatus) => void;
    let rejectStart!: (cause: unknown) => void;
    let reviewed: ManagedServerAuthorityInput | undefined;
    const api = {
      managedServerStatus: vi.fn(async () => observed),
      startManagedServer: vi.fn(
        (_name: string, authority?: ManagedServerAuthorityInput) => {
          reviewed = authority;
          return new Promise<ManagedServerStatus>((resolve, reject) => {
            resolveStart = resolve;
            rejectStart = reject;
          });
        },
      ),
      openManagedServerLoginItems: vi.fn(async () => undefined),
      enrollManagedServer: vi.fn(async () => undefined),
      commitManagedServerAutoStart: vi.fn(async () => observed),
      fetchDesktopSnapshot: vi.fn(async () => ({
        bootstrap,
        client: { deployments: [deployment] },
      })),
      listProviderAccounts: vi.fn(async () => []),
      listPackWireSources: vi.fn(async () => ({ plugins: [], target_packs: [] })),
      getInferenceSetupCatalog: vi.fn(async () => ({
        contractVersion: 1,
        defaultsVersion: "test",
        executionDefaults: {},
        providers: [],
      })),
      patchConfigComponents: vi.fn(async () => ({})),
      initLocalStandardRuntime: vi.fn(async () => ({ nodeDid: deployment.nodeDid })),
      startDesktopClient: vi.fn(async () => ({
        bootstrap,
        client: { deployments: [deployment] },
      })),
    };
    return {
      api,
      snapshot: { bootstrap: { ...bootstrap, initNodeName: "Forge" } },
      observe: (next: Partial<ManagedServerStatus>) => {
        observed = managedStatus(next);
      },
      rejectStart: (cause: unknown) => rejectStart(cause),
      readyStatus: () =>
        managedStatus({
          state: "running",
          nodeName: "Forge",
          nodeDid: "did:key:z6MkForgeIdentity0123456789",
          effectiveToolCeiling: reviewed!.toolCeiling,
          effectiveToolRoot: reviewed!.toolRoot ?? null,
          pairingReady: true,
        }),
      finishStart: () => {
        const ready = managedStatus({
          state: "running",
          nodeName: "Forge",
          nodeDid: "did:key:z6MkForgeIdentity0123456789",
          effectiveToolCeiling: reviewed!.toolCeiling,
          effectiveToolRoot: reviewed!.toolRoot ?? null,
          pairingReady: true,
        });
        observed = ready;
        resolveStart(ready);
      },
    };
  }

  it("waits for macOS approval and a slow boot, then names the node that is up", async () => {
    const run = firstRun();
    renderIn(
      testApp({ api: run.api, snapshot: run.snapshot }),
      <SetupScreen onDone={vi.fn()} />,
    );
    const next = screen.getByTestId("setup-next");
    await waitFor(() => expect(next).toBeEnabled());

    run.observe({ state: "starting", approvalRequired: true });
    await userEvent.click(next);

    const wait = await screen.findByTestId("setup-managed-server-wait", undefined, {
      timeout: 3_000,
    });
    expect(screen.getByTestId("setup-screen")).toHaveTextContent(
      "Waiting for macOS to allow Gents in the background",
    );
    expect(wait).toHaveTextContent("Login Items & Extensions");
    await userEvent.click(screen.getByTestId("setup-open-login-items"));
    expect(run.api.openManagedServerLoginItems).toHaveBeenCalledOnce();

    run.observe({ state: "starting" });
    await waitFor(
      () =>
        expect(screen.getByTestId("setup-screen")).toHaveTextContent(
          "Waiting for the background node to finish starting",
        ),
      { timeout: 3_000 },
    );
    expect(screen.queryByTestId("setup-open-login-items")).not.toBeInTheDocument();
    expect(screen.queryByText("Try again")).not.toBeInTheDocument();

    run.finishStart();
    await screen.findByRole("heading", { name: "Gents is ready" }, { timeout: 4_000 });
    const summary = screen.getByTestId("startup-step");
    expect(summary).toHaveTextContent(`${deployment.label} is running`);
    expect(summary).toHaveTextContent("Identitydid:key:z6MkFo");
    expect(summary).toHaveTextContent("Data");
    expect(screen.queryByTestId("setup-managed-server-wait")).not.toBeInTheDocument();

    await userEvent.click(screen.getByTestId("setup-continue"));
    await screen.findByRole("heading", { name: "Choose an inference provider" });
  }, 15_000);

  it("finishes on the agent it set up when a paired remote node is listed first", async () => {
    const run = firstRun();
    const remote = {
      ...deployment,
      nodeDid: "did:key:z6MkRemote",
      label: "Remote Node",
      source: "status",
    };
    run.api.fetchDesktopSnapshot = vi.fn(async () => ({
      bootstrap,
      client: { deployments: [remote, deployment] },
    }));
    renderIn(
      testApp({ api: run.api, snapshot: run.snapshot }),
      <SetupScreen onDone={vi.fn()} />,
    );
    const next = screen.getByTestId("setup-next");
    await waitFor(() => expect(next).toBeEnabled());
    await userEvent.click(next);
    run.finishStart();

    await screen.findByRole("heading", { name: "Gents is ready" }, { timeout: 4_000 });
    const summary = screen.getByTestId("startup-step");
    expect(summary).toHaveTextContent(`${deployment.label} is running`);
    expect(summary).not.toHaveTextContent("Remote Node");
  }, 15_000);

  it("does not provision an unrelated node when the initialized node is absent", async () => {
    const run = firstRun();
    const remote = {
      ...deployment,
      nodeDid: "did:key:unrelated",
      source: "enrollment",
    };
    run.api.fetchDesktopSnapshot = vi.fn(async () => ({
      bootstrap,
      client: { deployments: [remote] },
    }));
    renderIn(
      testApp({ api: run.api, snapshot: run.snapshot }),
      <SetupScreen onDone={vi.fn()} />,
    );
    const next = screen.getByTestId("setup-next");
    await waitFor(() => expect(next).toBeEnabled());
    await userEvent.click(next);
    run.finishStart();
    await screen.findByText(
      "The initialized node is not available in the client snapshot.",
    );
    expect(run.api.patchConfigComponents).not.toHaveBeenCalled();
    expect(
      screen.queryByRole("heading", { name: "Gents is ready" }),
    ).not.toBeInTheDocument();
  });

  it("keeps observing a runtime its start left migrating, then continues without restarting it", async () => {
    const run = firstRun();
    renderIn(
      testApp({ api: run.api, snapshot: run.snapshot }),
      <SetupScreen onDone={vi.fn()} />,
    );
    const next = screen.getByTestId("setup-next");
    await waitFor(() => expect(next).toBeEnabled());
    await userEvent.click(next);
    await waitFor(() => expect(run.api.startManagedServer).toHaveBeenCalled());

    run.observe({ state: "starting", runtimeBooting: true, nodeName: "Forge" });
    run.rejectStart(
      new BridgeInvokeError({
        code: "runtimeStillBooting",
        message:
          "the Gents runtime is still starting after 300 seconds, possibly migrating its data; it keeps starting in the background",
        retryable: false,
        endpoint: null,
      }),
    );

    await screen.findByText("Updating data…", undefined, { timeout: 5_000 });
    expect(screen.queryByText("Try again")).not.toBeInTheDocument();
    expect(
      screen.queryByText(/keeps starting in the background/),
    ).not.toBeInTheDocument();

    run.observe(run.readyStatus());
    await waitFor(() => expect(run.api.initLocalStandardRuntime).toHaveBeenCalled(), {
      timeout: 5_000,
    });
    expect(run.api.startManagedServer).toHaveBeenCalledOnce();
    expect(screen.queryByText("Try again")).not.toBeInTheDocument();
  }, 15_000);

  it("shows the step that failed, with its error and a way to try again", async () => {
    const run = firstRun();
    run.api.initLocalStandardRuntime.mockRejectedValueOnce(
      new Error("saved connections could not be written"),
    );
    renderIn(
      testApp({ api: run.api, snapshot: run.snapshot }),
      <SetupScreen onDone={vi.fn()} />,
    );
    const next = screen.getByTestId("setup-next");
    await waitFor(() => expect(next).toBeEnabled());
    await userEvent.click(next);
    await waitFor(() => expect(run.api.startManagedServer).toHaveBeenCalled());
    run.finishStart();

    await screen.findByText("saved connections could not be written");
    const failed = screen.getByTestId("startup-step");
    expect(failed.querySelector('[data-state="error"]')).not.toBeNull();
    expect(screen.getByRole("heading", { name: "Startup paused" })).toBeInTheDocument();
    expect(screen.getByText("Try again")).toBeInTheDocument();
    const logs = screen.getByRole("button", { name: "Where to find the logs" });
    expect(screen.queryByText(bootstrap.diagnosticsHint)).not.toBeVisible();
    await userEvent.click(logs);
    expect(screen.getByText(bootstrap.diagnosticsHint)).toBeVisible();
    expect(screen.queryByTestId("setup-continue")).not.toBeInTheDocument();
  });

  it("pauses on the finished card before moving on by itself", async () => {
    const run = firstRun();
    renderIn(
      testApp({ api: run.api, snapshot: run.snapshot }),
      <SetupScreen onDone={vi.fn()} />,
    );
    const next = screen.getByTestId("setup-next");
    await waitFor(() => expect(next).toBeEnabled());
    await userEvent.click(next);
    await waitFor(() => expect(run.api.startManagedServer).toHaveBeenCalled());
    run.finishStart();

    await screen.findByRole("heading", { name: "Gents is ready" }, { timeout: 4_000 });
    await new Promise((resolve) => setTimeout(resolve, 1_000));
    expect(screen.getByRole("heading", { name: "Gents is ready" })).toBeInTheDocument();
    await screen.findByRole(
      "heading",
      { name: "Choose an inference provider" },
      { timeout: 3_000 },
    );
  }, 10_000);

  describe("self-repair at launch", () => {
    const repairResult = (
      overrides: Partial<ManagedServerRepairResult> = {},
    ): ManagedServerRepairResult => ({
      repaired: [],
      outcome: "ready",
      blocker: null,
      savedPeers: [],
      localNodeLost: false,
      ...overrides,
    });
    const repairApp = (
      repairManagedServer: () => Promise<ManagedServerRepairResult>,
      calls: string[] = [],
    ) =>
      testApp({
        api: {
          managedServerStatus: vi.fn(async () => {
            calls.push("status");
            return managedStatus({ state: "running" });
          }),
          repairManagedServer,
        },
        supportsManagedServer: true,
      });

    it("launch_runs_the_repair_before_checking_the_node", async () => {
      const calls: string[] = [];
      const app = repairApp(async () => {
        calls.push("repair");
        return repairResult();
      }, calls);
      await app.lifecycle.initializeDesktop();
      expect(calls.slice(0, 2)).toEqual(["repair", "status"]);
    });

    it("a_blocked_repair_pauses_startup_with_its_reason", async () => {
      const app = repairApp(async () =>
        repairResult({ outcome: "blocked", blocker: "The folder is not writable." }),
      );
      await app.lifecycle.initializeDesktop();
      renderIn(app, <StartupScreen />);
      expect(screen.getByTestId("startup-error")).toHaveTextContent(
        "The folder is not writable.",
      );
    });

    it("try_again_runs_the_repair_again", async () => {
      const repair = vi.fn(async () =>
        repairResult({ outcome: "blocked", blocker: "Still stuck." }),
      );
      const app = repairApp(repair);
      await app.lifecycle.initializeDesktop();
      renderIn(app, <StartupScreen />);
      await userEvent.click(screen.getByTestId("startup-retry"));
      await waitFor(() => expect(repair).toHaveBeenCalledTimes(2));
    });

    it("a_blocked_repair_clears_earlier_notes", async () => {
      const app = repairApp(async () =>
        repairResult({
          outcome: "blocked",
          blocker: "Stuck.",
          repaired: ["Did a thing."],
        }),
      );
      app.stores.client.setState({ repairNotes: ["Old note."] });
      await app.lifecycle.initializeDesktop();
      expect(app.stores.client.getState().repairNotes).toEqual([]);
    });

    it("a_client_start_adds_what_it_set_aside_to_the_banner", async () => {
      const app = testApp({
        api: {
          startDesktopClient: vi.fn(async () => ({
            bootstrap,
            client: null,
            repairNote: "The desktop's local data was moved aside.",
          })),
        },
      });
      app.stores.client.setState({ repairNotes: ["Earlier note."] });
      await app.lifecycle.ensureDesktopClientStarted();
      expect(app.stores.client.getState().repairNotes).toEqual([
        "Earlier note.",
        "The desktop's local data was moved aside.",
      ]);
    });

    it("repair_notes_are_kept_for_the_banner", async () => {
      const peer = {
        peerId: "p",
        label: "Studio",
        nodeDid: "did:key:studio",
        addr: "a",
        source: "enrollment",
        graphql: null,
        local: false,
      };
      const app = repairApp(async () =>
        repairResult({ repaired: ["Moved the old folder aside."], savedPeers: [peer] }),
      );
      await app.lifecycle.initializeDesktop();
      expect(app.stores.client.getState().repairNotes).toEqual([
        "Moved the old folder aside.",
      ]);
      expect(app.stores.client.getState().repairPeers).toEqual([peer]);
    });
  });
});
