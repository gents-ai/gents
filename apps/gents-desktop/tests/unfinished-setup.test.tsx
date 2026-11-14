import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { renderIn, testApp } from "./app-fixture";

import type {
  ManagedServerAuthorityInput,
  ManagedServerResetResult,
  ManagedServerStatus,
} from "@source-inc/gents-desktop-client";

import { IncompatibleHomeScreen } from "../src/components/IncompatibleHomeScreen";
import { useStartup } from "../src/ui/hooks/useClient";
import { SetupScreen } from "../src/ui/screens/setup/SetupScreen";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";

const HOME = "/Users/test/.gents";

const unfinished = {
  ...bootstrap,
  initNodeName: "Forge",
  initConfigStatus: "configured" as const,
  nodeHomeExists: true,
  localSetupUnfinished: true,
  savedPeers: [],
};

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
    runtimeBooting: false,
    approvalRequired: false,
    error: null,
    ...overrides,
  };
}

function unfinishedPreview(): ManagedServerResetResult {
  return {
    managedHome: HOME,
    desktopHome: null,
    stores: [],
    confirmation: `RESET ${HOME} AND ARCHIVE LOCAL HISTORY`,
    deleteConfirmation: null,
    consequence: "The selected local state is moved into a timestamped backup.",
    completed: false,
    disposition: null,
    backupPath: null,
    plannedPaths: [`${HOME}/init.json`, `${HOME}/data`, `${HOME}/keys`],
    deletePaths: [],
    retiredPaths: [],
    retainedPaths: [],
    browserPaths: [],
    clearedBrowserPaths: [],
    reason: "unfinishedSetup",
    retiredPairings: 0,
  };
}

function harness() {
  let reviewed: ManagedServerAuthorityInput | undefined;
  const api = {
    managedServerStatus: vi.fn(async () => managedStatus()),
    startManagedServer: vi.fn(
      async (_name: string, authority?: ManagedServerAuthorityInput) => {
        reviewed = authority;
        return managedStatus({
          state: "running",
          nodeName: "Forge",
          nodeDid: deployment.nodeDid,
          effectiveToolCeiling: reviewed?.toolCeiling ?? null,
          effectiveToolRoot: reviewed?.toolRoot ?? null,
          pairingReady: true,
        });
      },
    ),
    resetManagedServer: vi.fn(async () => unfinishedPreview()),
    fetchDesktopSnapshot: vi.fn(async () => ({
      bootstrap: unfinished,
      client: { deployments: [] },
    })),
    listProviderAccounts: vi.fn(async () => []),
    getInferenceSetupCatalog: vi.fn(async () => ({
      contractVersion: 1,
      defaultsVersion: "test",
      executionDefaults: {},
      providers: [],
    })),
  };
  return {
    api,
    app: testApp({ api, snapshot: { bootstrap: unfinished } }),
  };
}

/* mirrors App.tsx: a reset preview replaces the setup screen */
function Recovery() {
  const startup = useStartup();
  return startup.incompatibleHome.report ? (
    <IncompatibleHomeScreen home={startup.incompatibleHome} error={startup.error} />
  ) : (
    <SetupScreen onDone={vi.fn()} />
  );
}

describe("a local setup that never finished", () => {
  it("explains the unfinished home instead of offering a name it cannot change", async () => {
    const { api, app } = harness();
    renderIn(app, <Recovery />);

    const block = await screen.findByTestId("setup-unfinished-home");
    expect(block).toHaveTextContent("An earlier setup on this computer didn’t finish");
    expect(block).toHaveTextContent("Forge");
    expect(screen.queryByLabelText("Node name")).toBeNull();

    await waitFor(() => expect(screen.getByTestId("setup-next")).toBeEnabled());
    await userEvent.click(screen.getByRole("button", { name: "Continue with Forge" }));
    await waitFor(() => expect(api.startManagedServer).toHaveBeenCalled());
    expect(api.startManagedServer.mock.calls[0][0]).toBe("Forge");
  });

  it("offers starting over through the reset preview, which names the unfinished home", async () => {
    const { api, app } = harness();
    renderIn(app, <Recovery />);

    await screen.findByTestId("setup-unfinished-home");
    await waitFor(() => expect(screen.getByTestId("setup-next")).toBeEnabled());
    await userEvent.click(
      screen.getByRole("button", { name: "Start over with a new name" }),
    );

    await waitFor(() => expect(api.resetManagedServer).toHaveBeenCalledWith());
    expect(
      await screen.findByRole("heading", { name: "An earlier setup didn’t finish" }),
    ).toBeInTheDocument();
    expect(screen.getByTestId("incompatible-home-unfinished")).toHaveTextContent(
      "never started",
    );
    expect(api.startManagedServer).not.toHaveBeenCalled();
  });
});
