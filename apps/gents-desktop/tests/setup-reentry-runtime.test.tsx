import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));

import {
  BridgeInvokeError,
  type DesktopApiAdapter,
  type ManagedServerStatus,
} from "@source-inc/gents-desktop-client";
import type { Shell } from "../src/ui/hooks/useShell";
import { setupErrorMessage } from "../src/ui/lib/providerLogin";
import { SetupScreen } from "../src/ui/screens/setup/SetupScreen";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";

type MockApi = Record<string, ReturnType<typeof vi.fn>>;

const AGENT = deployment.agentDid;
const RAW_FAILURE =
  "storing Claude credential: posting GraphQL mutation to http://127.0.0.1:9191/api/v0/graphql";

function status(overrides: Partial<ManagedServerStatus> = {}): ManagedServerStatus {
  return {
    state: "stopped",
    autoStart: true,
    agentName: "Local Agent",
    agentDid: AGENT,
    graphql: null,
    effectiveToolCeiling: "readwrite",
    effectiveToolRoot: "/tmp/work",
    suggestedToolRoot: "/tmp",
    pairingReady: false,
    error: null,
    ...overrides,
  } as ManagedServerStatus;
}

const claudeCatalog = {
  contractVersion: 1,
  defaultsVersion: "test",
  executionDefaults: {},
  providers: [
    {
      id: "anthropic",
      displayName: "Anthropic",
      description: "Claude subscription",
      authMethods: ["claude_oauth"],
      authOptions: [
        {
          method: "claude_oauth",
          displayName: "Claude sign-in",
          defaultEndpoint: "https://api.anthropic.com",
        },
      ],
      defaultAuthMethod: "claude_oauth",
      defaultEndpoint: "https://api.anthropic.com",
    },
  ],
};

const account = {
  credentialId: `claude-subscription:${AGENT}`,
  agentDid: AGENT,
  provider: "claude-subscription",
  accountId: "acct-1",
  planType: null,
  accessTokenExpiresAt: "2099-01-01T00:00:00Z",
  lastRefresh: null,
  enabled: true,
  pendingSave: false,
};

const NOT_SAVED = new BridgeInvokeError({
  code: "credentialNotSaved",
  message:
    "You are signed in to Claude, but Gents could not save the sign-in to the agent. Make sure the agent is running, then retry saving.",
  retryable: true,
  endpoint: null,
});

/* The bridge's held sign-in, observed through the provider account list. */
function holdFailedSignIn(api: MockApi) {
  let held = false;
  api.listProviderAccounts.mockImplementation(async () =>
    held ? [{ ...account, pendingSave: true }] : [],
  );
  api.claudeLogin.mockImplementation(async () => {
    held = true;
    throw NOT_SAVED;
  });
  api.retrySaveProviderAccount.mockImplementation(async () => {
    held = false;
    return account;
  });
}

function harness() {
  const api: MockApi = {
    getInferenceSetupCatalog: vi.fn().mockResolvedValue(claudeCatalog),
    listProviderAccounts: vi.fn().mockResolvedValue([]),
    fetchDesktopSnapshot: vi.fn().mockResolvedValue({
      bootstrap,
      client: { deployments: [deployment] },
    }),
    managedServerStatus: vi.fn(),
    startManagedServer: vi.fn(),
    claudeLogin: vi.fn(),
    cancelClaudeLogin: vi.fn().mockResolvedValue(undefined),
    retrySaveProviderAccount: vi.fn(),
  };
  const shell = {
    api: api as unknown as DesktopApiAdapter,
    snapshot: { bootstrap, client: { deployments: [deployment] } },
    deployments: [deployment],
    selectedDeployment: deployment,
    refreshSnapshot: vi.fn().mockResolvedValue(undefined),
    applyConfig: (run: (bridge: DesktopApiAdapter) => Promise<unknown>) =>
      run(api as unknown as DesktopApiAdapter),
  } as unknown as Shell;
  return { api, shell };
}

function reenter(shell: Shell) {
  return render(<SetupScreen shell={shell} initialStep="inference" onDone={vi.fn()} />);
}

beforeEach(() => vi.clearAllMocks());

describe("setup re-entry at the provider step", () => {
  it("starts the stopped managed runtime and waits for readiness before sign-in", async () => {
    const { api, shell } = harness();
    let started = false;
    api.managedServerStatus.mockImplementation(async () =>
      started ? status({ state: "running", pairingReady: true }) : status(),
    );
    api.startManagedServer.mockImplementation(async () => {
      started = true;
      return status({ state: "running" });
    });
    reenter(shell);

    expect(await screen.findByRole("button", { name: "Sign in" })).toBeVisible();
    expect(api.startManagedServer).toHaveBeenCalledTimes(1);
    expect(api.startManagedServer).toHaveBeenCalledWith("Local Agent");
    expect(api.claudeLogin).not.toHaveBeenCalled();
  });

  it("does not restart a runtime that is already serving", async () => {
    const { api, shell } = harness();
    api.managedServerStatus.mockResolvedValue(
      status({ state: "running", pairingReady: true }),
    );
    reenter(shell);
    expect(await screen.findByRole("button", { name: "Sign in" })).toBeVisible();
    expect(api.startManagedServer).not.toHaveBeenCalled();
  });

  it("refuses to offer OAuth when the runtime cannot be started, without leaking internals", async () => {
    const { api, shell } = harness();
    api.managedServerStatus.mockResolvedValue(status({ state: "failed" }));
    api.startManagedServer.mockRejectedValue(new Error(RAW_FAILURE));
    reenter(shell);

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(/local agent is not running/i);
    expect(alert.textContent).not.toMatch(/http|graphql|127\.0\.0\.1/i);
    expect(screen.queryByRole("button", { name: "Sign in" })).not.toBeInTheDocument();
    expect(api.claudeLogin).not.toHaveBeenCalled();

    // Try again reuses the same start + readiness owner.
    api.startManagedServer.mockResolvedValue(status({ state: "running" }));
    api.managedServerStatus
      .mockResolvedValueOnce(status({ state: "stopped" }))
      .mockResolvedValue(status({ state: "running", pairingReady: true }));
    await userEvent.setup().click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByRole("button", { name: "Sign in" })).toBeVisible();
  });

  it("keeps a completed sign-in whose save failed and retries the save without OAuth", async () => {
    const { api, shell } = harness();
    api.managedServerStatus.mockResolvedValue(
      status({ state: "running", pairingReady: true }),
    );
    holdFailedSignIn(api);
    reenter(shell);
    const user = userEvent.setup();

    await user.click(await screen.findByRole("button", { name: "Sign in" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "You are signed in to Claude",
    );
    await user.click(await screen.findByRole("button", { name: "Retry save" }));

    expect(await screen.findByText("Account connected")).toBeVisible();
    expect(api.retrySaveProviderAccount).toHaveBeenCalledWith(
      AGENT,
      "claude-subscription",
    );
    expect(api.claudeLogin).toHaveBeenCalledTimes(1);
    expect(
      screen.queryByRole("button", { name: "Retry save" }),
    ).not.toBeInTheDocument();
  });

  it("offers Retry save again after leaving and re-entering setup", async () => {
    const { api, shell } = harness();
    api.managedServerStatus.mockResolvedValue(
      status({ state: "running", pairingReady: true }),
    );
    holdFailedSignIn(api);
    const first = reenter(shell);
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Sign in" }));
    expect(await screen.findByRole("button", { name: "Retry save" })).toBeVisible();
    first.unmount();

    reenter(shell);
    await user.click(await screen.findByRole("button", { name: "Retry save" }));
    expect(await screen.findByText("Account connected")).toBeVisible();
    expect(api.claudeLogin).toHaveBeenCalledTimes(1);
    expect(api.retrySaveProviderAccount).toHaveBeenCalledWith(
      AGENT,
      "claude-subscription",
    );
  });

  it("shows a held sign-in on re-entry while the runtime is still down", async () => {
    const { api, shell } = harness();
    api.managedServerStatus.mockResolvedValue(status({ state: "failed" }));
    api.startManagedServer.mockRejectedValue(new Error(RAW_FAILURE));
    api.listProviderAccounts.mockResolvedValue([{ ...account, pendingSave: true }]);
    reenter(shell);

    expect(await screen.findByRole("button", { name: "Retry save" })).toBeVisible();
    expect(screen.queryByText("Account connected")).not.toBeInTheDocument();
    expect(api.claudeLogin).not.toHaveBeenCalled();
  });

  it("skips the runtime gate for a remote agent", async () => {
    const { api, shell } = harness();
    const remote = { ...deployment, agentDid: "did:key:zRemote", source: "enrolled" };
    (shell as unknown as { deployments: unknown[] }).deployments = [remote];
    render(
      <SetupScreen
        shell={shell}
        initialStep="inference"
        agentDid="did:key:zRemote"
        onDone={vi.fn()}
      />,
    );
    expect(await screen.findByRole("button", { name: "Sign in" })).toBeVisible();
    await waitFor(() => expect(api.getInferenceSetupCatalog).toHaveBeenCalled());
    expect(api.managedServerStatus).not.toHaveBeenCalled();
    expect(api.startManagedServer).not.toHaveBeenCalled();
  });
});

describe("setupErrorMessage", () => {
  it("replaces endpoint URLs and GraphQL internals with user-facing text", () => {
    expect(setupErrorMessage(new Error(RAW_FAILURE))).toBe(
      "Something went wrong. Try again.",
    );
    expect(
      setupErrorMessage(
        new BridgeInvokeError({
          code: "endpointUnreachable",
          message: "sending POST to http://127.0.0.1:9191/api/v0/graphql",
          retryable: true,
          endpoint: "http://127.0.0.1:9191",
        }),
      ),
    ).toBe("The agent is not reachable. Make sure it is running, then try again.");
  });

  it("keeps messages that are already user-facing", () => {
    expect(setupErrorMessage(new Error("API key is required"))).toBe(
      "API key is required",
    );
  });
});
