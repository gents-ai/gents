import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { DesktopApp } from "../src/hooks/desktopApp";
import { renderIn, testApp } from "./app-fixture";

const toast = vi.hoisted(() => vi.fn());
vi.mock("sonner", () => ({ toast }));

import type {
  DeploymentView,
  ManagedServerAuthorityInput,
  ManagedServerStatus,
} from "@source-inc/gents-desktop-client";
import { DEFAULT_NODE_NAME } from "@source-inc/gents-desktop-fleet";
import { AgentsScreen } from "../src/ui/screens/AgentsScreen";
import { SetupScreen } from "../src/ui/screens/setup/SetupScreen";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";

const FORGE_DID = "did:key:z6MkForgeIdentity0123456789";
const forgeBootstrap = {
  ...bootstrap,
  initNodeName: "Forge",
  initNodeDid: FORGE_DID,
};
const forge: DeploymentView = {
  ...deployment,
  peerId: "peer-forge",
  label: "Forge",
  nodeDid: FORGE_DID,
  source: "local-standard",
  node: {
    ...deployment.node,
    nodeDid: FORGE_DID,
    displayName: "Forge",
  },
};
const remote: DeploymentView = {
  ...deployment,
  peerId: "peer-remote",
  label: "Remote",
  nodeDid: "did:key:z6MkRemote",
  source: "enrollment",
  node: {
    ...deployment.node,
    nodeDid: "did:key:z6MkRemote",
    displayName: "Remote",
  },
};

function status(overrides: Partial<ManagedServerStatus> = {}): ManagedServerStatus {
  return {
    state: "running",
    autoStart: true,
    nodeName: "Forge",
    nodeDid: FORGE_DID,
    graphql: null,
    effectiveToolCeiling: "readwrite",
    effectiveToolRoot: "/tmp/work",
    suggestedToolRoot: "/tmp",
    pairingReady: true,
    approvalRequired: false,
    runtimeBooting: false,
    error: null,
    ...overrides,
  };
}

function fleet(listed: DeploymentView[], read: string[] = []) {
  const api = {
    managedServerStatus: vi.fn(async () => status()),
    startManagedServer: vi.fn(async () => status()),
    fetchDesktopSnapshot: vi.fn(async () => ({
      bootstrap: forgeBootstrap,
      client: { deployments: listed, operatorConfigAuthoritativeNodeDids: read },
    })),
    requestStatusEnrollment: vi.fn(async (_address: string) => ({
      requestId: "request-1",
    })),
    initLocalStandardRuntime: vi.fn(),
    renamePeer: vi.fn(async () => ({
      bootstrap: forgeBootstrap,
      client: { deployments: listed },
    })),
  };
  const app = testApp({
    api,
    snapshot: {
      bootstrap: forgeBootstrap,
      client: { deployments: listed, operatorConfigAuthoritativeNodeDids: read },
    },
    reportFailure: toast,
  });
  return { api, app };
}

async function openAddAgent(app: DesktopApp) {
  renderIn(app, <AgentsScreen />);
  await userEvent.click(screen.getByRole("button", { name: /Add node/ }));
  return screen.findByRole("dialog");
}

beforeEach(() => vi.clearAllMocks());

describe("Add node enrollment", () => {
  it("offers local setup after a remote-only enrollment and can return to the node list", async () => {
    const { app } = fleet([remote]);
    const snapshot = app.stores.client.getState().snapshot!;
    app.stores.client.setState({
      snapshot: {
        ...snapshot,
        bootstrap: {
          ...snapshot.bootstrap,
          initConfigStatus: "absent",
          initNodeDid: null,
          nodeHomeExists: false,
        },
      },
    });
    renderIn(app, <AgentsScreen />);
    await userEvent.click(screen.getByRole("button", { name: "Set up local node" }));
    expect(screen.getByText("Local node")).toBeVisible();
    expect(screen.queryByText("Remote connect")).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: /Back/ }));
    expect(screen.getByText("Remote")).toBeVisible();
  });

  it.each([[forge], [remote]])(
    "does not offer local reconnect for existing or removed local agents",
    async (listed) => {
      const { api, app } = fleet([listed]);
      const dialog = await openAddAgent(app);
      expect(
        within(dialog).queryByRole("button", { name: /reconnect/i }),
      ).not.toBeInTheDocument();
      expect(
        within(dialog).queryByRole("radio", { name: /Local node/ }),
      ).not.toBeInTheDocument();
      expect(within(dialog).getByLabelText("Node server")).toBeInTheDocument();
      expect(api.startManagedServer).not.toHaveBeenCalled();
      expect(api.initLocalStandardRuntime).not.toHaveBeenCalled();
    },
  );

  it.each(["local-standard", "enrollment"])(
    "hides impossible Remove for the managed agent with source %s",
    async (source) => {
      const { app } = fleet([{ ...forge, source }]);
      renderIn(app, <AgentsScreen />);
      await userEvent.click(screen.getByRole("button", { name: "Forge actions" }));
      expect(
        await screen.findByRole("menuitem", { name: "Rename" }),
      ).toBeInTheDocument();
      expect(
        screen.queryByRole("menuitem", { name: "Remove peer" }),
      ).not.toBeInTheDocument();
    },
  );

  it("retains Remove for a remote enrolled peer", async () => {
    const { app } = fleet([remote]);
    renderIn(app, <AgentsScreen />);
    await userEvent.click(screen.getByRole("button", { name: "Remote actions" }));
    expect(
      await screen.findByRole("menuitem", { name: "Remove peer" }),
    ).toBeInTheDocument();
  });

  it("shows a failed request's reason, and opens clean the next time", async () => {
    const { api, app } = fleet([forge]);
    api.requestStatusEnrollment.mockRejectedValueOnce(new Error("server refused"));
    let dialog = await openAddAgent(app);
    await userEvent.type(
      within(dialog).getByLabelText("Node server"),
      "server.example:9191",
    );
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Request enrolment" }),
    );
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(
      "server refused",
    );
    await userEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    await userEvent.click(screen.getByRole("button", { name: /Add node/ }));
    dialog = await screen.findByRole("dialog");
    expect(within(dialog).queryByRole("alert")).not.toBeInTheDocument();
  });

  it("retains initial remote enrollment", async () => {
    const { api, app } = fleet([forge]);
    api.requestStatusEnrollment.mockResolvedValue({ requestId: "request-1" });
    const dialog = await openAddAgent(app);
    await userEvent.type(
      within(dialog).getByLabelText("Node server"),
      "server.example:9191",
    );
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Request enrolment" }),
    );
    await waitFor(() =>
      expect(api.requestStatusEnrollment).toHaveBeenCalledWith("server.example:9191"),
    );
    expect(api.fetchDesktopSnapshot).toHaveBeenCalled();
  });
});

describe("the local node's Setup needed", () => {
  it("shows once the node's configuration is read and it has no inference", async () => {
    const { app } = fleet([forge], [forge.nodeDid]);
    renderIn(app, <AgentsScreen />);
    expect(await screen.findByRole("button", { name: "Setup needed" })).toBeVisible();
  });

  it("does not show before the configuration is read: what it lacks is unread", async () => {
    const { app } = fleet([{ ...forge, inferenceBackends: [] }]);
    renderIn(app, <AgentsScreen />);
    expect(await screen.findByText("Forge")).toBeVisible();
    expect(
      screen.queryByRole("button", { name: "Setup needed" }),
    ).not.toBeInTheDocument();
  });
});

describe("renaming a deployment", () => {
  it("renaming a deployment saves its trimmed label", async () => {
    const { api, app } = fleet([remote]);
    renderIn(app, <AgentsScreen />);

    await userEvent.click(screen.getByRole("button", { name: "Remote actions" }));
    await userEvent.click(await screen.findByRole("menuitem", { name: "Rename" }));
    const dialog = await screen.findByRole("dialog");
    const input = within(dialog).getByRole("textbox");
    await userEvent.clear(input);
    await userEvent.type(input, " Edge 2 ");
    await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));

    await waitFor(() => expect(api.renamePeer).toHaveBeenCalledTimes(1));
    expect(api.renamePeer).toHaveBeenCalledWith("peer-remote", "Edge 2");
  });
});

describe("first-run local agent name", () => {
  function setup(opts: { existingHome: boolean; runtimeName: string }) {
    let reviewed: ManagedServerAuthorityInput | undefined;
    const api = {
      managedServerStatus: vi.fn(async () =>
        status({ state: "disabled", nodeName: null, nodeDid: null }),
      ),
      startManagedServer: vi.fn(
        async (_name: string, authority?: ManagedServerAuthorityInput) => {
          reviewed = authority;
          return status({
            nodeName: opts.runtimeName,
            effectiveToolCeiling: reviewed!.toolCeiling,
            effectiveToolRoot: reviewed!.toolRoot ?? null,
          });
        },
      ),
      commitManagedServerAutoStart: vi.fn(async () => status()),
      fetchDesktopSnapshot: vi.fn(async () => ({
        bootstrap: forgeBootstrap,
        client: { deployments: [forge] },
      })),
      initLocalStandardRuntime: vi.fn(async () => ({ nodeDid: FORGE_DID })),
      startDesktopClient: vi.fn(async () => ({
        bootstrap: forgeBootstrap,
        client: { deployments: [forge] },
      })),
      listProviderAccounts: vi.fn(async () => []),
      getInferenceSetupCatalog: vi.fn(async () => ({
        contractVersion: 1,
        defaultsVersion: "test",
        executionDefaults: {},
        providers: [],
      })),
    };
    const snapshotBootstrap = opts.existingHome
      ? forgeBootstrap
      : {
          ...bootstrap,
          initNodeName: null,
          initNodeDid: null,
          nodeHomeExists: false,
        };
    renderIn(
      testApp({ api, snapshot: { bootstrap: snapshotBootstrap } }),
      <SetupScreen onDone={vi.fn()} />,
    );
    return { api };
  }

  it("shows an existing home's agent by name instead of asking for one it would ignore", async () => {
    const { api } = setup({ existingHome: true, runtimeName: "Forge" });
    const name = screen.getByLabelText("Node name");
    expect(name).toHaveValue("Forge");
    expect(name).toHaveAttribute("readonly");
    await userEvent.type(name, "Scout");
    expect(name).toHaveValue("Forge");

    const next = screen.getByTestId("setup-next");
    await waitFor(() => expect(next).toBeEnabled());
    await userEvent.click(next);
    await waitFor(() =>
      expect(api.initLocalStandardRuntime).toHaveBeenCalledWith(
        expect.objectContaining({ label: "Forge" }),
      ),
    );
    expect(api.startManagedServer).toHaveBeenCalledWith("Forge", expect.anything());
  });

  it("persists the entered name for a new home", async () => {
    const { api } = setup({ existingHome: false, runtimeName: "Scout" });
    const name = screen.getByLabelText("Node name");
    expect(name).toHaveValue(DEFAULT_NODE_NAME);
    await userEvent.clear(name);
    await userEvent.type(name, "Scout");
    const next = screen.getByTestId("setup-next");
    await waitFor(() => expect(next).toBeEnabled());
    await userEvent.click(next);

    await waitFor(() =>
      expect(api.initLocalStandardRuntime).toHaveBeenCalledWith(
        expect.objectContaining({ label: "Scout" }),
      ),
    );
    expect(api.startManagedServer).toHaveBeenCalledWith("Scout", expect.anything());
    expect(await screen.findByText(/Scout is running/)).toBeInTheDocument();
  });

  it("fails clearly instead of continuing under another agent's name", async () => {
    const { api } = setup({ existingHome: false, runtimeName: "Forge" });
    const name = screen.getByLabelText("Node name");
    await userEvent.clear(name);
    await userEvent.type(name, "Scout");
    const next = screen.getByTestId("setup-next");
    await waitFor(() => expect(next).toBeEnabled());
    await userEvent.click(next);

    expect(
      await screen.findByText(
        "This computer already has a local node named Forge, so Scout was not created. Go back to continue with Forge.",
      ),
    ).toBeInTheDocument();
    expect(api.initLocalStandardRuntime).not.toHaveBeenCalled();
    expect(api.fetchDesktopSnapshot).toHaveBeenCalled();
    expect(screen.queryByText(/Saved the connection/)).not.toBeInTheDocument();
    expect(screen.getByText("Try again")).toBeInTheDocument();
  });
});
