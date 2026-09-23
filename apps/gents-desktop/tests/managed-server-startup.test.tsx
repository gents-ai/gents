import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import type {
  DesktopApiAdapter,
  ManagedServerAuthorityInput,
  ManagedServerStatus,
} from "@source-inc/gents-desktop-client";

import { StartupScreen } from "../src/components/StartupScreen";
import { restoreManagedServer } from "../src/hooks/managedServerLifecycle";
import {
  awaitManagedServerSettled,
  type ManagedServerWait,
} from "../src/lib/managedServerStartup";
import type { Shell } from "../src/ui/hooks/useShell";
import { SetupScreen } from "../src/ui/screens/setup/SetupScreen";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";

function managedStatus(
  overrides: Partial<ManagedServerStatus> = {},
): ManagedServerStatus {
  return {
    state: "disabled",
    autoStart: false,
    agentName: null,
    agentDid: null,
    graphql: null,
    effectiveToolCeiling: null,
    effectiveToolRoot: null,
    suggestedToolRoot: "/Users/test",
    pairingReady: false,
    approvalRequired: false,
    error: null,
    ...overrides,
  };
}

describe("managed server startup waits", () => {
  it("keeps waiting while a slow runtime boots, then reports it running", async () => {
    const statuses = [
      managedStatus({ state: "starting" }),
      managedStatus({ state: "starting" }),
      managedStatus({ state: "running", agentDid: "did:key:slow" }),
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

  it("lets launch continue without the local agent while approval is pending", async () => {
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

  it("shows macOS approval guidance with a settings shortcut on the launch screen", async () => {
    const open = vi.fn(async () => undefined);
    const skip = vi.fn();
    render(
      <StartupScreen
        error={null}
        managedServerSupported
        managedServerWait={{ kind: "approval", since: Date.now() - 12_000 }}
        onOpenLoginItems={open}
        onRetry={vi.fn(async () => undefined)}
        onSkipManagedServerWait={skip}
        phase="checking-managed-server"
      />,
    );

    const screenText = screen.getByTestId("startup-screen");
    expect(screenText).toHaveTextContent(
      "Waiting for macOS to allow Gents in the background",
    );
    expect(screenText).toHaveTextContent("Login Items & Extensions");
    expect(screenText).toHaveTextContent("Waiting 12s");
    await userEvent.click(screen.getByTestId("startup-open-login-items"));
    expect(open).toHaveBeenCalledOnce();
    await userEvent.click(screen.getByTestId("startup-skip-managed-server-wait"));
    expect(skip).toHaveBeenCalledOnce();
  });

  it("names a booting runtime and how long it has waited instead of failing", () => {
    render(
      <StartupScreen
        error={null}
        managedServerSupported
        managedServerWait={{ kind: "booting", since: Date.now() - 75_000 }}
        onRetry={vi.fn(async () => undefined)}
        phase="checking-managed-server"
      />,
    );
    const screenText = screen.getByTestId("startup-screen");
    expect(screenText).toHaveTextContent(
      "Waiting for the background agent to finish starting",
    );
    expect(screenText).toHaveTextContent("Waiting 1m 15s");
    expect(screen.queryByTestId("startup-retry")).not.toBeInTheDocument();
  });
});

describe("first-run local agent startup", () => {
  function firstRun() {
    let observed = managedStatus();
    let resolveStart!: (status: ManagedServerStatus) => void;
    let reviewed: ManagedServerAuthorityInput | undefined;
    const api = {
      managedServerStatus: vi.fn(async () => observed),
      startManagedServer: vi.fn(
        (_name: string, authority?: ManagedServerAuthorityInput) => {
          reviewed = authority;
          return new Promise<ManagedServerStatus>((resolve) => {
            resolveStart = resolve;
          });
        },
      ),
      openManagedServerLoginItems: vi.fn(async () => undefined),
      commitManagedServerAutoStart: vi.fn(async () => observed),
      fetchDesktopSnapshot: vi.fn(async () => ({
        bootstrap,
        client: { deployments: [deployment] },
      })),
      listProviderAccounts: vi.fn(async () => []),
      getInferenceSetupCatalog: vi.fn(async () => ({
        contractVersion: 1,
        defaultsVersion: "test",
        executionDefaults: {},
        providers: [],
      })),
      patchConfigComponents: vi.fn(async () => ({})),
    };
    const shell = {
      api,
      snapshot: { bootstrap: { ...bootstrap, initAgentName: "Forge" } },
      deployments: [],
      applyConfig: (run: (bridge: DesktopApiAdapter) => Promise<unknown>) =>
        run(api as unknown as DesktopApiAdapter),
      refreshSnapshot: vi.fn(async () => undefined),
      onInitLocalRuntime: vi.fn(async () => undefined),
    } as unknown as Shell;
    return {
      api,
      shell,
      observe: (next: Partial<ManagedServerStatus>) => {
        observed = managedStatus(next);
      },
      finishStart: () => {
        const ready = managedStatus({
          state: "running",
          agentName: "Forge",
          agentDid: "did:key:z6MkForgeIdentity0123456789",
          effectiveToolCeiling: reviewed!.toolCeiling,
          effectiveToolRoot: reviewed!.toolRoot ?? null,
          pairingReady: true,
        });
        observed = ready;
        resolveStart(ready);
      },
    };
  }

  it("waits for macOS approval and a slow boot, then keeps the step log readable", async () => {
    const run = firstRun();
    render(<SetupScreen shell={run.shell} onDone={vi.fn()} />);
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
          "Waiting for the background agent to finish starting",
        ),
      { timeout: 3_000 },
    );
    expect(screen.queryByTestId("setup-open-login-items")).not.toBeInTheDocument();
    expect(screen.queryByText("Try again")).not.toBeInTheDocument();

    run.finishStart();
    await screen.findByRole("heading", { name: "Ready" }, { timeout: 3_000 });
    const log = screen.getByRole("list", { name: "Setup progress" });
    expect(log).toHaveTextContent("Start local agent");
    expect(log).toHaveTextContent("Forge is running as did:key:z6MkFo");
    expect(log).toHaveTextContent("Saved the local connection to Forge");
    expect(log).toHaveTextContent(`Connected securely to ${deployment.label}`);
    expect(log.querySelectorAll('[data-state="complete"]')).toHaveLength(3);
    expect(screen.queryByTestId("setup-managed-server-wait")).not.toBeInTheDocument();

    await userEvent.click(screen.getByTestId("setup-continue"));
    await screen.findByRole("heading", { name: "Choose an inference provider" });
  }, 15_000);

  it("pauses on the completed step log before moving on by itself", async () => {
    const run = firstRun();
    render(<SetupScreen shell={run.shell} onDone={vi.fn()} />);
    const next = screen.getByTestId("setup-next");
    await waitFor(() => expect(next).toBeEnabled());
    await userEvent.click(next);
    await waitFor(() => expect(run.api.startManagedServer).toHaveBeenCalled());
    run.finishStart();

    await screen.findByRole("heading", { name: "Ready" });
    await new Promise((resolve) => setTimeout(resolve, 1_000));
    expect(screen.getByRole("heading", { name: "Ready" })).toBeInTheDocument();
    await screen.findByRole(
      "heading",
      { name: "Choose an inference provider" },
      { timeout: 3_000 },
    );
  }, 10_000);
});
