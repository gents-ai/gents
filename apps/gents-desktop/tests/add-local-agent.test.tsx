import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

const toast = vi.hoisted(() => vi.fn());
vi.mock("sonner", () => ({ toast }));

import type {
  DesktopApiAdapter,
  DeploymentView,
  ManagedServerAuthorityInput,
  ManagedServerStatus,
} from "@source-inc/gents-desktop-client";
import type { Shell } from "../src/ui/hooks/useShell";
import { AgentsScreen } from "../src/ui/screens/AgentsScreen";
import { SetupScreen } from "../src/ui/screens/setup/SetupScreen";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";

const FORGE_DID = "did:key:z6MkForgeIdentity0123456789";
const forgeBootstrap = {
  ...bootstrap,
  initAgentName: "Forge",
  initAgentDid: FORGE_DID,
};
const forge: DeploymentView = {
  ...deployment,
  peerId: "peer-forge",
  label: "Forge",
  agentDid: FORGE_DID,
  source: "local-standard",
  agentPrincipal: {
    ...deployment.agentPrincipal,
    agentDid: FORGE_DID,
    displayName: "Forge",
  },
};
const remote: DeploymentView = {
  ...deployment,
  peerId: "peer-remote",
  label: "Remote",
  agentDid: "did:key:z6MkRemote",
  source: "enrollment",
  agentPrincipal: {
    ...deployment.agentPrincipal,
    agentDid: "did:key:z6MkRemote",
    displayName: "Remote",
  },
};

function status(overrides: Partial<ManagedServerStatus> = {}): ManagedServerStatus {
  return {
    state: "running",
    autoStart: true,
    agentName: "Forge",
    agentDid: FORGE_DID,
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

function fleet(listed: DeploymentView[]) {
  let deployments = listed;
  const api = {
    managedServerStatus: vi.fn(async () => status()),
    startManagedServer: vi.fn(async () => status()),
    fetchDesktopSnapshot: vi.fn(async () => ({
      bootstrap: forgeBootstrap,
      client: { deployments },
    })),
    requestStatusEnrollment: vi.fn(),
  };
  const shell = {
    api,
    snapshot: { bootstrap: forgeBootstrap, client: { deployments: listed } },
    deployments: listed,
    refreshSnapshot: vi.fn(async () => undefined),
    onInitLocalRuntime: vi.fn(async () => {
      deployments = [...listed, forge];
      return { agentDid: FORGE_DID };
    }),
  } as unknown as Shell;
  return {
    api,
    shell,
    lose: () => {
      deployments = listed;
    },
  };
}

async function openAddAgent(shell: Shell) {
  render(<AgentsScreen shell={shell} />);
  await userEvent.click(screen.getByRole("button", { name: /Add agent/ }));
  return screen.findByRole("dialog");
}

beforeEach(() => vi.clearAllMocks());

describe("Add agent on a desktop that already runs its local agent", () => {
  it("does not offer to create a second, differently named local agent", async () => {
    const { api, shell } = fleet([forge]);
    const dialog = await openAddAgent(shell);

    const local = within(dialog).getByRole("radio", { name: /Local agent/ });
    await waitFor(() => expect(local).toBeDisabled());
    expect(local).toHaveTextContent("Forge already runs on this computer");
    expect(within(dialog).getByRole("radio", { name: /Remote connect/ })).toBeChecked();
    expect(within(dialog).queryByLabelText("Name")).not.toBeInTheDocument();
    expect(api.startManagedServer).not.toHaveBeenCalled();
  });

  it("reconnects the removed local agent and reports success only once it is listed", async () => {
    const { api, shell } = fleet([remote]);
    const dialog = await openAddAgent(shell);

    const local = within(dialog).getByRole("radio", { name: /Local agent/ });
    await waitFor(() => expect(local).toBeEnabled());
    expect(local).toHaveTextContent("Reconnect Forge");
    await userEvent.click(local);
    await userEvent.click(within(dialog).getByRole("button", { name: "Reconnect" }));

    await waitFor(() => expect(toast).toHaveBeenCalledWith("Forge reconnected"));
    expect(api.startManagedServer).toHaveBeenCalledWith("Forge");
    expect(shell.onInitLocalRuntime).toHaveBeenCalledWith("Forge");
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  });

  it("keeps the dialog open with the reason when reconnecting fails, and a retry works", async () => {
    const { shell } = fleet([remote]);
    (shell.onInitLocalRuntime as ReturnType<typeof vi.fn>).mockRejectedValueOnce(
      new Error("saved connections could not be written"),
    );
    const dialog = await openAddAgent(shell);
    const local = within(dialog).getByRole("radio", { name: /Local agent/ });
    await waitFor(() => expect(local).toBeEnabled());
    await userEvent.click(local);
    await userEvent.click(within(dialog).getByRole("button", { name: "Reconnect" }));

    expect(await within(dialog).findByRole("alert")).toHaveTextContent(
      "Couldn't reconnect the local agent: saved connections could not be written",
    );
    expect(toast).not.toHaveBeenCalled();
    expect(local).toBeChecked();

    await userEvent.click(within(dialog).getByRole("button", { name: "Reconnect" }));
    await waitFor(() => expect(toast).toHaveBeenCalledWith("Forge reconnected"));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  });

  it("does not claim success when the agent never reaches the agent list", async () => {
    const { shell, lose } = fleet([remote]);
    (shell.onInitLocalRuntime as ReturnType<typeof vi.fn>).mockImplementationOnce(
      async () => {
        lose();
        return { agentDid: FORGE_DID };
      },
    );
    const dialog = await openAddAgent(shell);
    const local = within(dialog).getByRole("radio", { name: /Local agent/ });
    await waitFor(() => expect(local).toBeEnabled());
    await userEvent.click(local);
    await userEvent.click(within(dialog).getByRole("button", { name: "Reconnect" }));

    expect(await within(dialog).findByRole("alert")).toHaveTextContent(
      "does not appear in the agent list",
    );
    expect(toast).not.toHaveBeenCalled();
  });
});

describe("first-run local agent name", () => {
  function setup(opts: { existingHome: boolean; runtimeName: string }) {
    let reviewed: ManagedServerAuthorityInput | undefined;
    const api = {
      managedServerStatus: vi.fn(async () =>
        status({ state: "disabled", agentName: null, agentDid: null }),
      ),
      startManagedServer: vi.fn(
        async (_name: string, authority?: ManagedServerAuthorityInput) => {
          reviewed = authority;
          return status({
            agentName: opts.runtimeName,
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
          initAgentName: null,
          initAgentDid: null,
          agentHomeExists: false,
        };
    const shell = {
      api,
      snapshot: { bootstrap: snapshotBootstrap },
      deployments: [],
      applyConfig: (run: (bridge: DesktopApiAdapter) => Promise<unknown>) =>
        run(api as unknown as DesktopApiAdapter),
      refreshSnapshot: vi.fn(async () => undefined),
      onInitLocalRuntime: vi.fn(async () => ({ agentDid: FORGE_DID })),
    } as unknown as Shell;
    render(<SetupScreen shell={shell} onDone={vi.fn()} />);
    return { api, shell };
  }

  it("shows an existing home's agent by name instead of asking for one it would ignore", async () => {
    const { api, shell } = setup({ existingHome: true, runtimeName: "Forge" });
    const name = screen.getByLabelText("Agent name");
    expect(name).toHaveValue("Forge");
    expect(name).toHaveAttribute("readonly");
    await userEvent.type(name, "Scout");
    expect(name).toHaveValue("Forge");

    const next = screen.getByTestId("setup-next");
    await waitFor(() => expect(next).toBeEnabled());
    await userEvent.click(next);
    await waitFor(() => expect(shell.onInitLocalRuntime).toHaveBeenCalledWith("Forge"));
    expect(api.startManagedServer).toHaveBeenCalledWith("Forge", expect.anything());
  });

  it("persists the entered name for a new home", async () => {
    const { api, shell } = setup({ existingHome: false, runtimeName: "Scout" });
    const name = screen.getByLabelText("Agent name");
    await userEvent.clear(name);
    await userEvent.type(name, "Scout");
    const next = screen.getByTestId("setup-next");
    await waitFor(() => expect(next).toBeEnabled());
    await userEvent.click(next);

    await waitFor(() => expect(shell.onInitLocalRuntime).toHaveBeenCalledWith("Scout"));
    expect(api.startManagedServer).toHaveBeenCalledWith("Scout", expect.anything());
    expect(await screen.findByText(/Scout is running as/)).toBeInTheDocument();
  });

  it("fails clearly instead of continuing under another agent's name", async () => {
    const { shell } = setup({ existingHome: false, runtimeName: "Forge" });
    const name = screen.getByLabelText("Agent name");
    await userEvent.clear(name);
    await userEvent.type(name, "Scout");
    const next = screen.getByTestId("setup-next");
    await waitFor(() => expect(next).toBeEnabled());
    await userEvent.click(next);

    expect(
      await screen.findByText(
        "This computer already has a local agent named Forge, so Scout was not created. Go back to continue with Forge.",
      ),
    ).toBeInTheDocument();
    expect(shell.onInitLocalRuntime).not.toHaveBeenCalled();
    expect(shell.refreshSnapshot).toHaveBeenCalled();
    expect(screen.queryByText(/Saved the local connection/)).not.toBeInTheDocument();
    expect(screen.getByText("Try again")).toBeInTheDocument();
  });
});
