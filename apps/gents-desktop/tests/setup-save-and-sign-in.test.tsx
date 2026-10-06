import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { renderIn, testApp } from "./app-fixture";

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));

import type {
  DesktopApiAdapter,
  DesktopClientSnapshot,
} from "@source-inc/gents-desktop-client";
import { SetupScreen } from "../src/ui/screens/setup/SetupScreen";
import type { ProviderId } from "../src/ui/screens/setup/InferenceSetup";
import { createDesktopUiHarness } from "./ui-harness/desktopHarness";

const AGENT = "did:key:z6MkBombadilAgent";

/* The harness adapter without a managed runtime to gate on, with every call
   observable. */
function setup(
  overrides: Partial<DesktopApiAdapter> = {},
  harness = createDesktopUiHarness({ scenario: "empty-fleet" }),
) {
  const api = {
    ...harness.adapter,
    managedServerStatus: undefined,
    ...overrides,
  } as DesktopApiAdapter;
  for (const name of [
    "codexLogin",
    "claudeLogin",
    "grokLogin",
    "applyConfigComponents",
  ] as const)
    vi.spyOn(api, name);
  const app = testApp({ api, snapshot: null });
  return { api, app };
}

const login = (api: DesktopApiAdapter, provider: ProviderId) =>
  provider === "openai"
    ? api.codexLogin
    : provider === "anthropic"
      ? api.claudeLogin
      : api.grokLogin;

afterEach(() => vi.useRealTimers());

describe("setup provider sign-in", () => {
  it.each(["openai", "anthropic", "grok"] as const)(
    "starts the %s sign-in when its add-backend form opens",
    async (provider) => {
      const { api, app } = setup();
      renderIn(
        app,
        <SetupScreen
          initialStep="inference"
          purpose="add-backend"
          provider={provider}
          agentDid={AGENT}
          onCancel={vi.fn()}
          onDone={vi.fn()}
        />,
      );
      expect(await screen.findByText("Account connected")).toBeVisible();
      expect(login(api, provider)).toHaveBeenCalledTimes(1);
      expect(login(api, provider)).toHaveBeenCalledWith(AGENT);
    },
  );

  it("starts sign-in for a chosen provider, not for the preselected one", async () => {
    const { api, app } = setup();
    renderIn(
      app,
      <SetupScreen initialStep="inference" agentDid={AGENT} onDone={vi.fn()} />,
    );
    expect(await screen.findByRole("button", { name: "Sign in" })).toBeVisible();
    expect(api.codexLogin).not.toHaveBeenCalled();

    await userEvent.click(screen.getByTestId("setup-provider-anthropic"));
    expect(await screen.findByText("Account connected")).toBeVisible();
    expect(api.claudeLogin).toHaveBeenCalledTimes(1);
    expect(api.codexLogin).not.toHaveBeenCalled();
  });

  it("starts sign-in when the preselected provider is chosen", async () => {
    const { api, app } = setup();
    renderIn(
      app,
      <SetupScreen initialStep="inference" agentDid={AGENT} onDone={vi.fn()} />,
    );
    await userEvent.click(await screen.findByTestId("setup-provider-openai"));
    expect(await screen.findByText("Account connected")).toBeVisible();
    expect(api.codexLogin).toHaveBeenCalledTimes(1);
  });

  it("keeps a choice made before the account lookup resolves, once", async () => {
    let resolveAccounts: (accounts: []) => void = () => {};
    const { api, app } = setup({
      listProviderAccounts: vi.fn(
        () => new Promise<[]>((resolve) => (resolveAccounts = resolve)),
      ),
      cancelClaudeLogin: vi.fn().mockResolvedValue(undefined),
    });
    let rejectLogin: (cause: Error) => void = () => {};
    vi.mocked(api.claudeLogin).mockImplementation(
      () => new Promise((_, reject) => (rejectLogin = reject)),
    );
    renderIn(
      app,
      <SetupScreen
        initialStep="inference"
        purpose="add-backend"
        provider="anthropic"
        agentDid={AGENT}
        onCancel={vi.fn()}
        onDone={vi.fn()}
      />,
    );
    await userEvent.click(await screen.findByRole("button", { name: "Sign in" }));
    resolveAccounts([]);
    rejectLogin(new Error("Claude sign-in was cancelled"));
    expect(await screen.findByRole("alert")).toHaveTextContent("cancelled");
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Sign in" })).toBeEnabled(),
    );
    expect(api.claudeLogin).toHaveBeenCalledTimes(1);
  });

  it("starts sign-in for the preselected provider chosen during the account lookup", async () => {
    let resolveAccounts: (accounts: []) => void = () => {};
    const { api, app } = setup({
      listProviderAccounts: vi.fn(
        () => new Promise<[]>((resolve) => (resolveAccounts = resolve)),
      ),
    });
    renderIn(
      app,
      <SetupScreen initialStep="inference" agentDid={AGENT} onDone={vi.fn()} />,
    );
    await userEvent.click(await screen.findByTestId("setup-provider-openai"));
    expect(api.codexLogin).not.toHaveBeenCalled();
    resolveAccounts([]);
    expect(await screen.findByText("Account connected")).toBeVisible();
    expect(api.codexLogin).toHaveBeenCalledTimes(1);
  });

  it("starts sign-in when the OAuth connection method is chosen", async () => {
    const { api, app } = setup();
    renderIn(
      app,
      <SetupScreen initialStep="inference" agentDid={AGENT} onDone={vi.fn()} />,
    );
    const user = userEvent.setup();
    const method = () => screen.getByRole("combobox", { name: "Connection method" });
    await screen.findByRole("button", { name: "Sign in" });
    await user.click(method());
    await user.click(await screen.findByRole("option", { name: "OpenAI API key" }));
    expect(api.codexLogin).not.toHaveBeenCalled();
    await user.click(method());
    await user.click(await screen.findByRole("option", { name: "ChatGPT sign-in" }));
    expect(await screen.findByText("Account connected")).toBeVisible();
    expect(api.codexLogin).toHaveBeenCalledTimes(1);
  });

  it("leaves sign-in to a click when the account lookup fails", async () => {
    const { api, app } = setup({
      listProviderAccounts: vi.fn().mockRejectedValue(new Error("runtime not serving")),
    });
    renderIn(
      app,
      <SetupScreen
        initialStep="inference"
        purpose="add-backend"
        provider="anthropic"
        agentDid={AGENT}
        onCancel={vi.fn()}
        onDone={vi.fn()}
      />,
    );
    await userEvent.click(await screen.findByRole("button", { name: "Sign in" }));
    expect(await screen.findByText("Account connected")).toBeVisible();
    expect(api.claudeLogin).toHaveBeenCalledTimes(1);
  });

  it("does not sign in again to a provider that is already connected", async () => {
    const { api, app } = setup({
      listProviderAccounts: vi.fn().mockResolvedValue([
        {
          credentialId: "credential-claude",
          agentDid: AGENT,
          provider: "claude-subscription",
          accountId: null,
          planType: null,
          accessTokenExpiresAt: "2099-01-01T00:00:00Z",
          lastRefresh: null,
          enabled: true,
          pendingSave: false,
        },
      ]),
    });
    renderIn(
      app,
      <SetupScreen
        initialStep="inference"
        purpose="add-backend"
        provider="anthropic"
        agentDid={AGENT}
        onCancel={vi.fn()}
        onDone={vi.fn()}
      />,
    );
    expect(await screen.findByLabelText("Account label")).toBeVisible();
    expect(screen.getByRole("button", { name: "Sign in" })).toBeVisible();
    expect(screen.queryByText("Account connected")).not.toBeInTheDocument();
    expect(api.claudeLogin).not.toHaveBeenCalled();
  });
});

describe("the provider catalog", () => {
  it("offers to read the catalog again after a failed read, and then goes on", async () => {
    const harness = createDesktopUiHarness({ scenario: "empty-fleet" });
    const read = harness.adapter.getInferenceSetupCatalog.bind(harness.adapter);
    const { app } = setup(
      {
        getInferenceSetupCatalog: vi
          .fn()
          .mockRejectedValueOnce(new Error("catalog offline"))
          .mockImplementation(read),
      },
      harness,
    );
    renderIn(
      app,
      <SetupScreen
        initialStep="inference"
        purpose="add-backend"
        provider="anthropic"
        agentDid={AGENT}
        onCancel={vi.fn()}
        onDone={vi.fn()}
      />,
    );
    expect(await screen.findByRole("alert")).toHaveTextContent("catalog offline");
    expect(screen.queryByText("Loading provider options…")).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByText("Account connected")).toBeVisible();
  });
});

describe("add another account", () => {
  const KIND = {
    openai: "chatgpt-codex",
    anthropic: "claude-subscription",
    grok: "xai-oauth",
  } as const;
  const stored = (provider: string) => ({
    credentialId: "credential-personal",
    agentDid: AGENT,
    provider,
    accountId: null,
    planType: null,
    accessTokenExpiresAt: "2099-01-01T00:00:00Z",
    lastRefresh: null,
    enabled: true,
    pendingSave: false,
    accountRef: null,
    label: "Personal",
  });
  const signedIn = (
    result: string,
    hint: string | null = null,
    accountRef: string | null = "acct-2",
  ) =>
    vi.fn().mockResolvedValue({
      docId: "credential-doc",
      credentialId: "credential-work",
      agentDid: AGENT,
      provider: "claude-subscription",
      accountId: null,
      chatgptPlanType: null,
      isFedramp: false,
      accessTokenExpiresAt: "2099-01-01T00:00:00Z",
      enabled: true,
      signIn: { result, label: "Work", accountRef, hint },
    });
  function addForm(
    provider: keyof typeof KIND,
    overrides: Partial<DesktopApiAdapter> = {},
  ) {
    const { api, app } = setup({
      listProviderAccounts: vi.fn().mockResolvedValue([stored(KIND[provider])]),
      ...overrides,
    });
    const onDone = vi.fn();
    renderIn(
      app,
      <SetupScreen
        initialStep="inference"
        purpose="add-backend"
        provider={provider}
        agentDid={AGENT}
        onCancel={vi.fn()}
        onDone={onDone}
      />,
    );
    return { api, onDone };
  }

  it("signs a labelled account in once and closes when it was added", async () => {
    const { api, onDone } = addForm("anthropic", { claudeLogin: signedIn("added") });
    const user = userEvent.setup();
    await user.type(await screen.findByLabelText("Account label"), "Work");
    await user.click(screen.getByRole("button", { name: "Sign in" }));
    await waitFor(() => expect(onDone).toHaveBeenCalledTimes(1));
    expect(api.claudeLogin).toHaveBeenCalledTimes(1);
    expect(api.claudeLogin).toHaveBeenCalledWith(AGENT, null, "Work");
    expect(api.applyConfigComponents).not.toHaveBeenCalled();
  });

  it("draws the hint after a refreshed sign-in and stays open", async () => {
    const hint = "Signed in to Work again. To add another account, sign out first.";
    const { onDone } = addForm("anthropic", {
      claudeLogin: signedIn("refreshed", hint),
    });
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Sign in" }));
    expect(await screen.findByText(hint)).toBeVisible();
    expect(onDone).not.toHaveBeenCalled();
  });

  it("keeps Sign in offered after a sign-in refreshes the original account", async () => {
    const hint = "This is the account already stored as Personal.";
    const { api, onDone } = addForm("anthropic", {
      claudeLogin: signedIn("refreshed", hint, null),
    });
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Sign in" }));
    expect(await screen.findByText(hint)).toBeVisible();
    expect(screen.queryByText("Account connected")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Sign in" })).toBeVisible();
    expect(onDone).not.toHaveBeenCalled();
    expect(api.applyConfigComponents).not.toHaveBeenCalled();
  });

  it("says no account was added when a refresh has no hint", async () => {
    const { api, onDone } = addForm("anthropic", {
      claudeLogin: signedIn("refreshed", null, null),
    });
    await userEvent.click(await screen.findByRole("button", { name: "Sign in" }));
    expect(
      await screen.findByText(
        "This sign-in refreshed the account stored as Work. No account was added.",
      ),
    ).toBeVisible();
    expect(screen.getByRole("button", { name: "Sign in" })).toBeVisible();
    expect(onDone).not.toHaveBeenCalled();
    expect(api.applyConfigComponents).not.toHaveBeenCalled();
  });

  it("closes after Retry save stores an added account", async () => {
    const work = {
      ...stored(KIND.anthropic),
      credentialId: "credential-work",
      label: "Work",
    };
    const { api, onDone } = addForm("anthropic", {
      listProviderAccounts: vi
        .fn()
        .mockResolvedValue([stored(KIND.anthropic), { ...work, pendingSave: true }]),
      retrySaveProviderAccount: vi
        .fn()
        .mockResolvedValue({ ...work, accountRef: "acct-2" }),
    });
    await userEvent.click(await screen.findByRole("button", { name: "Retry save" }));
    await waitFor(() => expect(onDone).toHaveBeenCalledTimes(1));
    expect(api.retrySaveProviderAccount).toHaveBeenCalledWith(
      AGENT,
      "claude-subscription",
    );
    expect(screen.queryByText("Account connected")).not.toBeInTheDocument();
    expect(api.applyConfigComponents).not.toHaveBeenCalled();
  });

  it("keeps Sign in offered after Retry save refreshes the original account", async () => {
    const personal = stored(KIND.anthropic);
    const { api, onDone } = addForm("anthropic", {
      listProviderAccounts: vi
        .fn()
        .mockResolvedValue([personal, { ...personal, pendingSave: true }]),
      retrySaveProviderAccount: vi.fn().mockResolvedValue(personal),
    });
    await userEvent.click(await screen.findByRole("button", { name: "Retry save" }));
    expect(
      await screen.findByText(
        "This sign-in refreshed the account stored as Personal. No account was added.",
      ),
    ).toBeVisible();
    expect(screen.queryByText("Account connected")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Sign in" })).toBeVisible();
    expect(
      screen.queryByRole("button", { name: "Retry save" }),
    ).not.toBeInTheDocument();
    expect(onDone).not.toHaveBeenCalled();
    expect(api.applyConfigComponents).not.toHaveBeenCalled();
  });

  it("offers no Cancel while Retry save runs: Cancel stops a sign-in", async () => {
    let finish = (_: unknown) => {};
    const work = {
      ...stored(KIND.anthropic),
      credentialId: "credential-work",
      label: "Work",
    };
    const { api } = addForm("anthropic", {
      listProviderAccounts: vi
        .fn()
        .mockResolvedValue([stored(KIND.anthropic), { ...work, pendingSave: true }]),
      retrySaveProviderAccount: vi.fn(
        () => new Promise((resolve) => (finish = resolve)),
      ) as DesktopApiAdapter["retrySaveProviderAccount"],
      cancelClaudeLogin: vi.fn(),
    });
    await userEvent.click(await screen.findByRole("button", { name: "Retry save" }));
    expect(await screen.findByRole("button", { name: /Saving…/ })).toBeDisabled();
    expect(screen.queryByRole("button", { name: "Cancel" })).toBeNull();
    expect(screen.queryByText("Waiting…")).toBeNull();
    finish({ ...work, accountRef: "acct-2" });
    await waitFor(() => expect(api.retrySaveProviderAccount).toHaveBeenCalledOnce());
    expect(api.cancelClaudeLogin).not.toHaveBeenCalled();
  });

  it("refuses a label another account of the provider shows", async () => {
    const { api } = addForm("anthropic", { claudeLogin: signedIn("added") });
    const user = userEvent.setup();
    await user.type(await screen.findByLabelText("Account label"), " Personal ");
    await user.click(screen.getByRole("button", { name: "Sign in" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Personal");
    expect(api.claudeLogin).not.toHaveBeenCalled();
  });

  it.each(["openai", "anthropic", "grok"] as const)(
    "adds another %s account",
    async (provider) => {
      const name =
        provider === "openai"
          ? "codexLogin"
          : provider === "anthropic"
            ? "claudeLogin"
            : "grokLogin";
      const { api, onDone } = addForm(provider, { [name]: signedIn("added") });
      await userEvent.click(await screen.findByRole("button", { name: "Sign in" }));
      await waitFor(() => expect(onDone).toHaveBeenCalledTimes(1));
      expect(login(api, provider)).toHaveBeenCalledWith(AGENT);
      expect(api.applyConfigComponents).not.toHaveBeenCalled();
    },
  );
});

describe("setup save", () => {
  /* #2068: the runtime confirms the rebound default behavior only after it
     reconciles, starts the slot and replicates readiness back. A confirmation
     that lands after the old five-second budget must still complete the first
     save. */
  it("completes on the first click when readiness arrives after five seconds", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    let appliedAt: number | null = null;
    const harness = createDesktopUiHarness({ scenario: "empty-fleet" });
    await harness.adapter.initLocalStandardRuntime({
      label: "Forge",
      dangerouslyOverwrite: false,
      reset: false,
    });
    const { api, app } = setup(
      {
        async applyConfigComponents(request) {
          appliedAt = Date.now();
          return harness.adapter.applyConfigComponents(request);
        },
        async fetchDesktopSnapshot(): Promise<DesktopClientSnapshot> {
          const snapshot = await harness.adapter.fetchDesktopSnapshot();
          if (appliedAt === null || Date.now() - appliedAt >= 6_000) return snapshot;
          return {
            ...snapshot,
            client: snapshot.client && {
              ...snapshot.client,
              deployments: snapshot.client.deployments.map((deployment) => ({
                ...deployment,
                behaviorReadiness: {
                  ...deployment.behaviorReadiness,
                  behaviors: deployment.behaviorReadiness.behaviors.map((behavior) => ({
                    state: "unavailable" as const,
                    behaviorId: behavior.behaviorId,
                    reason: "backend_disabled" as const,
                  })),
                },
              })),
            },
          };
        },
      },
      harness,
    );
    const onDone = vi.fn();
    renderIn(
      app,
      <SetupScreen initialStep="inference" agentDid={AGENT} onDone={onDone} />,
    );
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    await user.click(await screen.findByTestId("setup-provider-local"));
    await user.click(screen.getByRole("button", { name: "Connect and find models" }));
    await user.click(
      await screen.findByRole("option", { name: "GLM-5.3-Flash-NVFP4" }),
    );
    await user.click(screen.getByTestId("setup-save-inference"));

    await vi.advanceTimersByTimeAsync(7_000);
    await waitFor(() => expect(onDone).toHaveBeenCalledTimes(1));
    expect(api.applyConfigComponents).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });
});
