import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import {
  BridgeInvokeError,
  type DesktopApiAdapter,
  type DesktopClientSnapshot,
  type DesktopClientUpdatedListenerFactory,
  type ManagedServerResetResult,
  type ManagedServerStatus,
} from "@source-inc/gents-desktop-client";

import App from "../src/App";
import type { Shell } from "../src/ui/hooks/useShell";
import { SetupScreen } from "../src/ui/screens/setup/SetupScreen";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";

const HOME = "/Users/test/.gents";
const DESKTOP = "/Users/test/Library/Application Support/gents/desktop";

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

function preview(
  overrides: Partial<ManagedServerResetResult> = {},
): ManagedServerResetResult {
  return {
    managedHome: HOME,
    desktopHome: DESKTOP,
    stores: [
      {
        scope: "runtime",
        path: `${HOME}/data`,
        detail: `${HOME}/data was created by an older Gents version.`,
        older: true,
        unsafeKey: false,
      },
    ],
    confirmation: `RESET ${HOME} AND ARCHIVE LOCAL HISTORY`,
    deleteConfirmation: `DELETE ${HOME} PERMANENTLY`,
    consequence: "The old home is moved into a timestamped backup.",
    completed: false,
    disposition: null,
    backupPath: null,
    plannedPaths: [`${HOME}/data`, `${HOME}/keys`, `${DESKTOP}/principal.ed25519.key`],
    deletePaths: [
      `${HOME}/data`,
      `${HOME}/keys/local.key`,
      `${HOME}/packs`,
      `${DESKTOP}/principal.ed25519.key`,
    ],
    retiredPaths: [],
    retainedPaths: [`${HOME}/grok-port-home`],
    ...overrides,
  };
}

function incompatible(message = "the store was created by an older Gents version") {
  return new BridgeInvokeError({
    code: "incompatibleLocalStore",
    message,
    retryable: false,
    endpoint: null,
  });
}

function snapshot(configured: boolean): DesktopClientSnapshot {
  return {
    bootstrap: {
      ...bootstrap,
      clientStateExists: configured,
      savedPeers: configured
        ? [
            {
              peerId: deployment.peerId,
              label: deployment.label,
              agentDid: deployment.agentDid,
              addr: deployment.addr,
              source: deployment.source,
              graphql: deployment.graphql,
            },
          ]
        : [],
    },
    client: null,
  };
}

function harness(api: Partial<DesktopApiAdapter>) {
  const resetManagedServer = vi.fn(
    async (confirmation?: string, disposition?: "archive" | "delete") => {
      if (!confirmation) return preview();
      return preview({
        completed: true,
        disposition: disposition ?? "archive",
        backupPath:
          disposition === "delete"
            ? null
            : "/Users/test/.gents-backup-20260924T000000.000Z",
        retiredPaths: [`${HOME}/data`, `${HOME}/init.json`],
      });
    },
  );
  const full = {
    fetchDesktopSnapshot: vi.fn(async () => snapshot(false)),
    fetchSessionSnapshot: vi.fn(async () => null),
    setSelectedAgent: vi.fn(async () => undefined),
    startDesktopClient: vi.fn(async () => snapshot(false)),
    shutdownDesktopClient: vi.fn(async () => snapshot(false)),
    startManagedServer: vi.fn(),
    resetManagedServer,
    quitDesktop: vi.fn(async () => undefined),
    getInferenceSetupCatalog: vi.fn(async () => ({
      contractVersion: 1,
      defaultsVersion: "test",
      providers: [],
    })),
    ...api,
  } as unknown as DesktopApiAdapter;
  const listenToUpdates: DesktopClientUpdatedListenerFactory = async () => () => {};
  render(<App bridge={{ api: full, listenToUpdates, supportsManagedServer: true }} />);
  return { api: full, resetManagedServer };
}

describe("a home this version cannot open", () => {
  it("offers back up, delete, or keep when startup finds a refused runtime store", async () => {
    const run = harness({
      managedServerStatus: vi.fn(async () =>
        managedStatus({
          state: "failed",
          error: `${HOME}/data was created by an older Gents version.`,
          errorCode: "incompatibleLocalStore",
        }),
      ),
    });

    const panel = await screen.findByTestId("incompatible-home");
    expect(panel).toHaveTextContent(
      "This home was created by an older Gents version. 0.19 is a breaking release and can't open it.",
    );
    expect(panel).toHaveTextContent(`${HOME}/data`);
    expect(screen.getByTestId("incompatible-home-retained")).toHaveTextContent(
      `${HOME}/grok-port-home`,
    );
    expect(run.resetManagedServer).toHaveBeenCalledWith();
    expect(screen.getByTestId("incompatible-home-backup")).toHaveTextContent(
      "Back up and start fresh",
    );
    expect(screen.getByTestId("incompatible-home-delete")).toHaveTextContent(
      "Delete and start fresh",
    );
    expect(screen.getByTestId("incompatible-home-keep")).toHaveTextContent(
      "Keep it and quit",
    );
  });

  it("backs the home up, shows where, then continues to fresh onboarding", async () => {
    let refused = true;
    const run = harness({
      managedServerStatus: vi.fn(async () =>
        refused
          ? managedStatus({ state: "failed", errorCode: "incompatibleLocalStore" })
          : managedStatus(),
      ),
    });

    await userEvent.click(await screen.findByTestId("incompatible-home-backup"));
    expect(run.resetManagedServer).toHaveBeenLastCalledWith(
      `RESET ${HOME} AND ARCHIVE LOCAL HISTORY`,
      "archive",
    );
    expect(
      await screen.findByTestId("incompatible-home-backup-path"),
    ).toHaveTextContent("/Users/test/.gents-backup-20260924T000000.000Z");

    refused = false;
    await userEvent.click(screen.getByTestId("incompatible-home-continue"));
    await waitFor(() => {
      expect(screen.getByTestId("setup-screen")).toBeInTheDocument();
    });
    expect(screen.queryByTestId("incompatible-home")).not.toBeInTheDocument();
  });

  it("deletes only after an explicit confirmation", async () => {
    const run = harness({
      managedServerStatus: vi.fn(async () =>
        managedStatus({ state: "failed", errorCode: "incompatibleLocalStore" }),
      ),
    });

    expect(await screen.findByTestId("incompatible-home-planned")).toHaveTextContent(
      `${HOME}/keys`,
    );
    await userEvent.click(await screen.findByTestId("incompatible-home-delete"));
    expect(run.resetManagedServer).toHaveBeenCalledTimes(1);
    expect(
      screen.getByTestId("incompatible-home-delete-client-note"),
    ).toHaveTextContent("pairings with remote agents");
    expect(screen.getByTestId("incompatible-home-delete-paths")).toHaveTextContent(
      `${HOME}/keys/local.key`,
    );
    expect(
      screen.getByTestId("incompatible-home-delete-installed-note"),
    ).toHaveTextContent("packs and plugins");
    expect(
      screen.getByTestId("incompatible-home-delete-confirmation"),
    ).toHaveTextContent("can't be undone");
    await userEvent.click(screen.getByTestId("incompatible-home-delete-cancel"));
    expect(run.resetManagedServer).toHaveBeenCalledTimes(1);

    await userEvent.click(screen.getByTestId("incompatible-home-delete"));
    await userEvent.click(screen.getByTestId("incompatible-home-delete-confirm"));
    expect(run.resetManagedServer).toHaveBeenLastCalledWith(
      `DELETE ${HOME} PERMANENTLY`,
      "delete",
    );
    expect(await screen.findByTestId("incompatible-home-done")).toHaveTextContent(
      "Old home deleted",
    );
  });

  it("keeps the home untouched and quits", async () => {
    const run = harness({
      managedServerStatus: vi.fn(async () =>
        managedStatus({ state: "failed", errorCode: "incompatibleLocalStore" }),
      ),
    });

    await userEvent.click(await screen.findByTestId("incompatible-home-keep"));
    expect(run.api.quitDesktop).toHaveBeenCalledOnce();
    expect(run.resetManagedServer).toHaveBeenCalledTimes(1);
    expect(run.resetManagedServer).toHaveBeenCalledWith();
  });

  it("offers the panel when a restart hits the refused store", async () => {
    const restartManagedServer = vi.fn(async () => {
      throw incompatible();
    });
    const run = harness({
      managedServerStatus: vi.fn(async () =>
        managedStatus({
          state: "failed",
          agentName: "Forge",
          effectiveToolCeiling: "readwrite",
          effectiveToolRoot: "/Users/test",
          error: "The background agent keeps exiting before it becomes ready.",
        }),
      ),
      restartManagedServer,
    });

    await userEvent.click(await screen.findByTestId("startup-restart-managed-server"));
    await screen.findByTestId("incompatible-home");
    expect(restartManagedServer).toHaveBeenCalledWith("Forge", {
      toolCeiling: "readwrite",
      toolRoot: "/Users/test",
    });
    expect(run.resetManagedServer).toHaveBeenCalledWith();
  });

  it("offers the panel when the desktop client store is the one refused", async () => {
    const run = harness({
      managedServerStatus: vi.fn(async () => managedStatus({ state: "stopped" })),
      fetchDesktopSnapshot: vi.fn(async () => snapshot(true)),
      startDesktopClient: vi.fn(async () => {
        throw incompatible("The desktop client store was created by an older version");
      }),
    });
    run.resetManagedServer.mockImplementation(async () =>
      preview({
        managedHome: null,
        stores: [
          {
            scope: "client",
            path: `${DESKTOP}/node`,
            detail: "created by an older Gents version",
            older: true,
            unsafeKey: false,
            unsafeKey: false,
          },
        ],
        retainedPaths: [],
      }),
    );

    const panel = await screen.findByTestId("incompatible-home");
    expect(panel).toHaveTextContent("Desktop app data");
    expect(panel).toHaveTextContent(`${DESKTOP}/node`);
  });

  it("does not offer deletion for a store another version wrote", async () => {
    const run = harness({
      managedServerStatus: vi.fn(async () =>
        managedStatus({ state: "failed", errorCode: "incompatibleLocalStore" }),
      ),
    });
    run.resetManagedServer.mockImplementation(async () =>
      preview({
        stores: [
          {
            scope: "runtime",
            path: `${HOME}/data`,
            detail: "written by a different Gents version",
            older: false,
            unsafeKey: false,
          },
        ],
        deleteConfirmation: null,
        deletePaths: [],
      }),
    );

    expect(
      await screen.findByTestId("incompatible-home-different-version"),
    ).toHaveTextContent("possibly a newer one");
    expect(screen.queryByTestId("incompatible-home-delete")).not.toBeInTheDocument();
    expect(screen.getByTestId("incompatible-home-backup")).toBeInTheDocument();
    expect(screen.getByTestId("incompatible-home-keep")).toBeInTheDocument();
  });

  it("explains keys written with unsafe permissions and offers all three choices", async () => {
    const run = harness({
      managedServerStatus: vi.fn(async () =>
        managedStatus({ state: "failed", errorCode: "incompatibleLocalStore" }),
      ),
    });
    run.resetManagedServer.mockImplementation(async () =>
      preview({
        stores: [
          {
            scope: "runtime",
            path: `${HOME}/keys`,
            detail: "keys readable by other users",
            older: true,
            unsafeKey: true,
          },
        ],
      }),
    );

    expect(await screen.findByTestId("incompatible-home-unsafe-key")).toHaveTextContent(
      "This home's keys were created by an older Gents version with unsafe file permissions",
    );
    expect(screen.getByTestId("incompatible-home-backup")).toBeInTheDocument();
    expect(screen.getByTestId("incompatible-home-delete")).toBeInTheDocument();
    expect(screen.getByTestId("incompatible-home-keep")).toBeInTheDocument();
  });

  it("keeps an unrelated failure on the ordinary startup error", async () => {
    const run = harness({
      managedServerStatus: vi.fn(async () =>
        managedStatus({ state: "failed", error: "the agent crashed" }),
      ),
    });

    expect(await screen.findByTestId("startup-screen")).toHaveTextContent(
      "the agent crashed",
    );
    expect(screen.queryByTestId("incompatible-home")).not.toBeInTheDocument();
    expect(run.resetManagedServer).not.toHaveBeenCalled();
  });
});

describe("setup start on a home this version cannot open", () => {
  it("hands the typed start failure to the incompatible-home flow", async () => {
    const adopt = vi.fn(async () => true);
    const api = {
      managedServerStatus: vi.fn(async () => managedStatus()),
      startManagedServer: vi.fn(async () => {
        throw incompatible();
      }),
      fetchDesktopSnapshot: vi.fn(async () => snapshot(false)),
      getInferenceSetupCatalog: vi.fn(async () => ({
        contractVersion: 1,
        defaultsVersion: "test",
        providers: [],
      })),
    };
    const shell = {
      api,
      snapshot: { bootstrap: { ...bootstrap, initAgentName: "Forge" } },
      deployments: [],
      refreshSnapshot: vi.fn(async () => undefined),
      onInitLocalRuntime: vi.fn(async () => undefined),
      incompatibleHome: { adopt },
    } as unknown as Shell;
    render(<SetupScreen shell={shell} onDone={vi.fn()} />);
    const next = screen.getByTestId("setup-next");
    await waitFor(() => expect(next).toBeEnabled());

    await userEvent.click(next);

    await waitFor(() => expect(adopt).toHaveBeenCalledOnce());
    const [error] = adopt.mock.calls[0] as unknown as [BridgeInvokeError];
    expect(error.code).toBe("incompatibleLocalStore");
    expect(shell.onInitLocalRuntime).not.toHaveBeenCalled();
  });
});
