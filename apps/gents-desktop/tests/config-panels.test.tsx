import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";
import type { Shell } from "../src/ui/hooks/useShell";
import { AgentPanel } from "../src/ui/screens/agent/AgentPanel";
import {
  BehaviorEditor,
  BehaviorsPanel,
  newBehaviorView,
} from "../src/ui/screens/agent/BehaviorsPanel";
import { ContextsPanel } from "../src/ui/screens/agent/ContextsPanel";
import { EventSourcesPanel } from "../src/ui/screens/agent/EventSourcesPanel";
import { InferencePanel } from "../src/ui/screens/agent/InferencePanel";
import {
  ProfileEditor,
  ProfilesPanel,
  newProfileDocument,
} from "../src/ui/screens/agent/ProfilesPanel";
import { SetupScreen } from "../src/ui/screens/setup/SetupScreen";
import { SchedulesPanel } from "../src/ui/screens/agent/SchedulesPanel";
import { SkillsPanel } from "../src/ui/screens/agent/SkillsPanel";
import { TasksPanel } from "../src/ui/screens/agent/TasksPanel";
import { ToolServicesPanel } from "../src/ui/screens/agent/ToolServicesPanel";
import { ToolsPanel } from "../src/ui/screens/agent/ToolsPanel";
import { TriggersPanel } from "../src/ui/screens/agent/TriggersPanel";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";

type MockApi = Record<string, ReturnType<typeof vi.fn>>;

function harness() {
  const api: MockApi = {
    saveAgentConfig: vi.fn().mockResolvedValue({}),
    setDefaultBehavior: vi.fn().mockResolvedValue({}),
    saveBehaviorConfig: vi.fn().mockResolvedValue({}),
    patchConfigComponents: vi.fn().mockResolvedValue({}),
    applyConfigComponents: vi.fn().mockResolvedValue({}),
    saveBackendConfig: vi.fn().mockResolvedValue({}),
    deleteBackendConfig: vi.fn().mockResolvedValue({}),
    probeInferenceEndpoint: vi
      .fn()
      .mockResolvedValue({ reachable: true, models: ["model-a"] }),
    getInferenceBackendRecommendation: vi.fn().mockResolvedValue({
      defaultsVersion: "test",
      summary: "Gents recommends balanced sampling.",
      contextWindow: null,
      maxOutputTokens: null,
      temperature: { recommended: 0.7, min: 0, max: 2, step: 0.05 },
      topP: { recommended: 1, min: 0, max: 1, step: 0.05 },
      reasoningEffort: null,
      maxConcurrent: { recommended: 1, min: 1, max: null },
    }),
    listProviderAccounts: vi.fn().mockResolvedValue([]),
    disconnectProviderAccount: vi.fn().mockResolvedValue(undefined),
    saveInferenceProfileConfig: vi.fn().mockResolvedValue({}),
    saveToolsConfig: vi.fn().mockResolvedValue({}),
    saveToolServiceConfig: vi.fn().mockResolvedValue({}),
    testToolService: vi.fn().mockResolvedValue({
      serviceId: "service-a",
      endpoint: "http://localhost:7331/mcp",
      status: "ok",
      toolCount: 0,
      tools: [],
      error: null,
    }),
    saveSkillConfig: vi.fn().mockResolvedValue({}),
    saveTaskConfig: vi.fn().mockResolvedValue({}),
    runTask: vi.fn().mockResolvedValue({ requestId: "request-a" }),
    saveScheduleConfig: vi.fn().mockResolvedValue({}),
    runSchedule: vi.fn().mockResolvedValue({ requestId: "request-schedule" }),
    saveEventSourceConfig: vi.fn().mockResolvedValue({}),
    saveTriggerConfig: vi.fn().mockResolvedValue({}),
    deleteBehaviorConfig: vi.fn().mockResolvedValue({}),
    deleteContextConfig: vi.fn().mockResolvedValue({}),
    deleteToolsConfig: vi.fn().mockResolvedValue({}),
    deleteToolServiceConfig: vi.fn().mockResolvedValue({}),
    deleteSkillConfig: vi.fn().mockResolvedValue({}),
    deleteTaskConfig: vi.fn().mockResolvedValue({}),
    deleteScheduleConfig: vi.fn().mockResolvedValue({}),
    deleteEventSourceConfig: vi.fn().mockResolvedValue({}),
    deleteTriggerConfig: vi.fn().mockResolvedValue({}),
    deleteInferenceProfileConfig: vi.fn().mockResolvedValue({}),
    fetchOperationsSnapshot: vi.fn().mockResolvedValue(null),
  };
  const refreshSnapshot = vi.fn().mockResolvedValue(undefined);
  const shell = {
    api: api as unknown as DesktopApiAdapter,
    snapshot: { bootstrap },
    saveAgentConfig: api.saveAgentConfig,
    saveBehaviorConfig: api.saveBehaviorConfig,
    applyConfig: (run: (bridge: DesktopApiAdapter) => Promise<unknown>) =>
      run(api as unknown as DesktopApiAdapter),
    refreshSnapshot,
    runTask: async (request: Parameters<typeof api.runTask>[0]) => {
      const result = await api.runTask(request);
      await refreshSnapshot();
      return result;
    },
    runSchedule: async (request: Parameters<typeof api.runSchedule>[0]) => {
      const result = await api.runSchedule(request);
      await refreshSnapshot();
      return result;
    },
    captureComposeIntent: () => 0,
    acceptsComposeIntent: (captured: number) => captured === 0,
  } as unknown as Shell;
  return { api, shell };
}

async function replace(label: string, value: string) {
  const user = userEvent.setup();
  const field = screen.getByLabelText(label);
  await user.clear(field);
  await user.type(field, value);
  return user;
}

function expectFields(labels: string[]) {
  for (const label of labels) {
    expect(screen.getAllByText(label, { selector: "label" }).length).toBeGreaterThan(0);
  }
}

beforeEach(() => vi.clearAllMocks());

describe("configuration panels", () => {
  it("continues an existing local Gents home instead of creating a new identity", async () => {
    const { shell } = harness();
    render(<SetupScreen shell={shell} onDone={vi.fn()} />);
    expect(
      screen.getByText("Continue the agent already on this computer."),
    ).toBeVisible();
    expect(
      screen.getByText(/Found an existing Gents home for Local Agent/),
    ).toBeVisible();
    expect(screen.getByDisplayValue("Local Agent")).toBeVisible();
  });

  it("clears a prior agent sign-in while the next account lookup fails", async () => {
    const { api, shell } = harness();
    api.getInferenceSetupCatalog = vi.fn().mockResolvedValue({
      contractVersion: 1,
      defaultsVersion: "test",
      executionDefaults: {},
      providers: [
        {
          id: "openai",
          displayName: "OpenAI",
          description: "OpenAI",
          authMethods: ["chat_gpt_oauth"],
          authOptions: [
            {
              method: "chat_gpt_oauth",
              displayName: "ChatGPT sign-in",
              defaultEndpoint: "https://chatgpt.com/backend-api/codex",
            },
          ],
          defaultAuthMethod: "chat_gpt_oauth",
          defaultEndpoint: "https://chatgpt.com/backend-api/codex",
        },
      ],
    });
    api.listProviderAccounts
      .mockResolvedValueOnce([
        {
          provider: "chatgpt-codex",
          enabled: true,
          credentialId: "agent-a-credential",
        },
      ])
      .mockRejectedValueOnce(new Error("agent B lookup failed"));
    const view = render(
      <SetupScreen
        shell={shell}
        initialStep="inference"
        agentDid="did:test:agent-a"
        onDone={vi.fn()}
      />,
    );
    expect(await screen.findByText("Account connected")).toBeVisible();
    view.rerender(
      <SetupScreen
        shell={shell}
        initialStep="inference"
        agentDid="did:test:agent-b"
        onDone={vi.fn()}
      />,
    );
    expect(await screen.findByRole("button", { name: "Sign in" })).toBeVisible();
    expect(screen.queryByText("Account connected")).not.toBeInTheDocument();
  });

  it("uses advertised context bounds when reopening a profile with a smaller saved override", async () => {
    const { api, shell } = harness();
    api.getInferenceBackendRecommendation.mockResolvedValue({
      defaultsVersion: "fixture",
      summary: "",
      contextWindow: { recommended: 272000, min: 1, max: 872000 },
      maxOutputTokens: null,
      temperature: null,
      topP: null,
      reasoningEffort: null,
      maxConcurrent: { recommended: 8, min: 1, max: null },
    });
    const profile = deployment.inferenceProfiles.find(
      (row) => row.profile_id === "profile-a",
    )!;
    const configured = {
      ...deployment,
      inferenceProfiles: deployment.inferenceProfiles.map((row) => ({
        ...row,
        context_window: 300000,
      })),
      inferenceBackends: deployment.inferenceBackends.map((backend) => ({
        ...backend,
        advertisedModels: [
          {
            model_name: profile.model_name,
            display_name: null,
            context_window: 272000,
            max_context_window: 872000,
            max_output_tokens: null,
            reasoning_efforts: null,
          },
        ],
      })),
    };
    render(<ProfilesPanel shell={shell} deployment={configured} item="profile-a" />);
    await waitFor(() =>
      expect(api.getInferenceBackendRecommendation).toHaveBeenCalledWith(
        expect.objectContaining({ contextWindow: 272000, maxContextWindow: 872000 }),
      ),
    );
    expect(api.saveInferenceProfileConfig).not.toHaveBeenCalled();
    expect(screen.getByLabelText("Context window", { exact: false })).toHaveValue(
      300000,
    );
    const user = userEvent.setup();
    await user.clear(screen.getByLabelText("Display name"));
    await user.type(screen.getByLabelText("Display name"), "Renamed profile");
    await user.click(screen.getByRole("button", { name: "Save", exact: true }));
    await waitFor(() => expect(api.applyConfigComponents).toHaveBeenCalled());
    expect(
      api.applyConfigComponents.mock.calls[0]![0].document.inference_profiles[0]
        .context_window,
    ).toBe(300000);
  });

  it.each([false, true])(
    "does not backfill Grok sampling on rename (existing sampling=%s)",
    async (existingSampling) => {
      const { api, shell } = harness();
      api.getInferenceBackendRecommendation.mockResolvedValue({
        defaultsVersion: "fixture",
        summary: "",
        contextWindow: null,
        maxOutputTokens: null,
        reasoningEffort: null,
        temperature: { recommended: 0.7, min: 0, max: 2, step: 0.05 },
        topP: { recommended: 0.95, min: 0, max: 1, step: 0.05 },
        maxConcurrent: { recommended: 8, min: 1, max: null },
      });
      const configured = {
        ...deployment,
        inferenceBackends: deployment.inferenceBackends.map((row) => ({
          ...row,
          providerKind: "XaiGrokOAuth",
        })),
        inferenceProfiles: deployment.inferenceProfiles.map((row) => ({
          ...row,
          sampling_id: existingSampling ? "sampling-custom" : null,
        })),
        inferenceSampling: existingSampling
          ? [
              {
                agent_did: deployment.agentDid,
                sampling_id: "sampling-custom",
                temperature: 0.4,
                top_p: null,
              },
            ]
          : [],
      };
      render(<ProfilesPanel shell={shell} deployment={configured} item="profile-a" />);
      await waitFor(() =>
        expect(api.getInferenceBackendRecommendation).toHaveBeenCalled(),
      );
      const user = userEvent.setup();
      await user.clear(screen.getByLabelText("Display name"));
      await user.type(screen.getByLabelText("Display name"), "Renamed Grok");
      await user.click(screen.getByRole("button", { name: "Save", exact: true }));
      await waitFor(() => expect(api.applyConfigComponents).toHaveBeenCalled());
      const document = api.applyConfigComponents.mock.calls[0]![0].document;
      if (existingSampling) {
        expect(document.inference_sampling[0]).toMatchObject({
          temperature: 0.4,
          top_p: null,
        });
      } else {
        expect(document.inference_sampling).toBeUndefined();
        expect(document.inference_profiles[0].sampling_id).toBeNull();
      }
    },
  );

  it("does not starve model defaults while equivalent snapshots refresh", async () => {
    const { api, shell } = harness();
    const view = render(
      <ProfilesPanel shell={shell} deployment={deployment} item="profile-a" />,
    );
    for (let refresh = 0; refresh < 5; refresh++) {
      await act(() => new Promise<void>((resolve) => setTimeout(resolve, 50)));
      view.rerender(
        <ProfilesPanel
          shell={shell}
          deployment={{
            ...deployment,
            inferenceBackends: deployment.inferenceBackends.map((backend) => ({
              ...backend,
            })),
          }}
          item="profile-a"
        />,
      );
    }
    expect(api.getInferenceBackendRecommendation).toHaveBeenCalledTimes(1);
    expect(screen.getByLabelText("Temperature")).toBeVisible();
  });

  it("refreshes subscription catalogs with authenticated discovery and hides credential IDs", async () => {
    const { api, shell } = harness();
    api.listProviderAccounts.mockResolvedValue([
      {
        provider: "xai-oauth",
        enabled: true,
        accountId: "person@example.test",
        label: "Grok 2",
        accountRef: "g-2",
        credentialId: "private-credential-id",
        planType: null,
        accessTokenExpiresAt: new Date(Date.now() + 3600000).toISOString(),
      },
    ]);
    api.discoverInferenceModels = vi.fn().mockResolvedValue({
      reachable: true,
      models: [{ advertised: { model_name: "grok-4.6" } }],
    });
    render(
      <InferencePanel
        shell={shell}
        item="backend-a"
        deployment={{
          ...deployment,
          inferenceBackends: [
            {
              ...deployment.inferenceBackends[0]!,
              providerKind: "XaiGrokOAuth",
              endpoint: "https://cli-chat-proxy.grok.com/v1",
              probeStatus: "healthy",
              models: ["grok-4.5"],
              accountRef: "g-2",
            },
          ],
        }}
      />,
    );
    expect(await screen.findByText("person@example.test")).toBeVisible();
    expect(screen.queryByText("private-credential-id")).not.toBeInTheDocument();
    expect(
      screen.getByText("Subscription", { selector: "[data-slot=badge]" }),
    ).toBeVisible();
    expect(screen.queryByLabelText("OpenAI wire API")).not.toBeInTheDocument();
    expect(screen.getByLabelText("Connect timeout seconds")).toHaveValue("");
    await userEvent
      .setup()
      .click(screen.getByRole("button", { name: "Refresh models" }));
    expect(await screen.findByText("grok-4.6")).toBeVisible();
    expect(api.discoverInferenceModels).toHaveBeenCalledWith(
      expect.objectContaining({
        agentDid: deployment.agentDid,
        provider: "grok",
        authMethod: "grok_oauth",
        apiKey: null,
        accountRef: "g-2",
      }),
    );
    expect(api.probeInferenceEndpoint).not.toHaveBeenCalled();
  });

  it("discards subscription discovery when the edited connection changes", async () => {
    const { api, shell } = harness();
    let resolveDiscovery!: (value: unknown) => void;
    api.discoverInferenceModels = vi.fn().mockReturnValue(
      new Promise((resolve) => {
        resolveDiscovery = resolve;
      }),
    );
    render(
      <InferencePanel
        shell={shell}
        item="backend-a"
        deployment={{
          ...deployment,
          inferenceBackends: [
            {
              ...deployment.inferenceBackends[0]!,
              providerKind: "XaiGrokOAuth",
              endpoint: "https://old.example.test/v1",
              models: ["saved-model"],
            },
          ],
        }}
      />,
    );
    await userEvent
      .setup()
      .click(screen.getByRole("button", { name: "Refresh models" }));
    await replace("Endpoint", "https://new.example.test/v1");
    await act(async () => {
      resolveDiscovery({
        reachable: true,
        models: [{ advertised: { model_name: "stale-model" } }],
      });
    });
    expect(screen.queryByText("stale-model")).not.toBeInTheDocument();
    expect(screen.getByText("saved-model")).toBeVisible();
  });

  describe("subscription backends and the account each references", () => {
    const claudeBackend = (backendId: string, accountRef: string | null) => ({
      ...deployment.inferenceBackends[0]!,
      backendId,
      name: backendId,
      providerKind: "ClaudeCliSubscription" as const,
      endpoint: "claude-cli://subscription",
      accountRef,
    });
    const claudeDeployment = {
      ...deployment,
      inferenceBackends: [
        claudeBackend("claude", null),
        claudeBackend("claude-2", "acct-2"),
        claudeBackend("claude-3", "acct-other"),
      ],
    };
    const claudeAccount = (
      credentialId: string,
      accountRef: string | null,
      accountId: string,
      enabled = true,
    ) => ({
      credentialId,
      agentDid: deployment.agentDid,
      provider: "claude-subscription",
      accountId,
      planType: null,
      accessTokenExpiresAt: "2099-01-01T00:00:00Z",
      lastRefresh: null,
      enabled,
      pendingSave: false,
      accountRef,
      label: `label-${credentialId}`,
    });

    it("never borrows another account for a backend in the list", async () => {
      const { api, shell } = harness();
      api.listProviderAccounts.mockResolvedValue([
        claudeAccount("cred-original", null, "original-identity", false),
        claudeAccount("cred-2", "acct-2", "second-identity"),
      ]);
      render(<InferencePanel shell={shell} deployment={claudeDeployment} />);
      expect(await screen.findAllByText(/· signed in$/)).toHaveLength(1);
      expect(screen.getAllByText(/· disabled$/)).toHaveLength(1);
      expect(screen.getAllByText(/account not on this node/)).toHaveLength(1);
    });

    for (const [item, credentialId, identity] of [
      ["claude", "cred-original", "original-identity"],
      ["claude-2", "cred-2", "second-identity"],
    ] as const) {
      it(`shows and disconnects the account ${item} references`, async () => {
        const { api, shell } = harness();
        api.listProviderAccounts.mockResolvedValue([
          claudeAccount("cred-original", null, "original-identity"),
          claudeAccount("cred-2", "acct-2", "second-identity"),
        ]);
        render(
          <InferencePanel shell={shell} deployment={claudeDeployment} item={item} />,
        );
        expect(await screen.findByText(identity)).toBeVisible();
        const user = userEvent.setup();
        await user.click(screen.getByRole("button", { name: "Disconnect" }));
        await user.click(screen.getByRole("button", { name: "Disconnect now" }));
        expect(api.disconnectProviderAccount).toHaveBeenCalledWith(
          deployment.agentDid,
          credentialId,
        );
      });
    }

    for (const [item, auth] of [
      ["claude-2", { kind: "principal_oauth", account_ref: "acct-2" }],
      ["claude", { kind: "principal_oauth" }],
    ] as const) {
      it(`saving ${item} keeps its account reference`, async () => {
        const { api, shell } = harness();
        api.listProviderAccounts.mockResolvedValue([]);
        render(
          <InferencePanel shell={shell} deployment={claudeDeployment} item={item} />,
        );
        const user = await replace("Name", `${item} edited`);
        await user.click(screen.getByRole("button", { name: "Save" }));
        await waitFor(() => expect(api.patchConfigComponents).toHaveBeenCalledTimes(1));
        const { patches } = api.patchConfigComponents.mock.calls[0][0];
        expect(patches[0].changes.auth).toEqual(auth);
      });
    }

    it("says a backend's account is not on this node and offers no reconnect", async () => {
      const { api, shell } = harness();
      api.listProviderAccounts.mockResolvedValue([
        claudeAccount("cred-original", null, "original-identity"),
      ]);
      render(
        <InferencePanel shell={shell} deployment={claudeDeployment} item="claude-3" />,
      );
      expect(await screen.findByText(/account not on this node/)).toBeVisible();
      expect(
        screen.queryByRole("button", { name: /connect/i }),
      ).not.toBeInTheDocument();
      expect(screen.queryByText("original-identity")).not.toBeInTheDocument();
    });

    describe("each account is its own row with its label and state, never expired", () => {
      const row = (
        backendId: string,
        name: string,
        providerKind: "ClaudeCliSubscription" | "XaiGrokOAuth" | "ChatGptCodex",
        accountRef: string | null,
      ) => ({ ...claudeBackend(backendId, accountRef), name, providerKind });
      const keyed = (backendId: string, fields: object) => ({
        ...deployment.inferenceBackends[0]!,
        backendId,
        name: backendId,
        ...fields,
      });
      const rowsDeployment = {
        ...deployment,
        inferenceBackends: [
          row("claude", "Claude", "ClaudeCliSubscription", null),
          row("claude-work", "Work", "ClaudeCliSubscription", "acct-2"),
          row("grok", "Grok", "XaiGrokOAuth", null),
          row("grok-side", "Side", "XaiGrokOAuth", "g-2"),
          row("chatgpt", "ChatGPT", "ChatGptCodex", null),
          row("chatgpt-team", "Team", "ChatGptCodex", "c-2"),
          keyed("openrouter", { providerKind: "OpenRouter", apiKeyConfigured: true }),
          keyed("openrouter-env", {
            providerKind: "OpenRouter",
            apiKeyEnvVar: "OPENROUTER_API_KEY",
          }),
          keyed("local", {}),
          keyed("local-2", {}),
        ],
      };
      const account = (
        provider: string,
        accountRef: string | null,
        label: string,
        fields: object = {},
      ) => ({
        ...claudeAccount(
          `private-credential-id-${label}`,
          accountRef,
          `${label}-identity`,
        ),
        provider,
        label,
        ...fields,
      });
      const accounts = [
        account("claude-subscription", null, "Personal", {
          accessTokenExpiresAt: "2001-01-01T00:00:00Z",
        }),
        account("claude-subscription", "acct-2", "Work"),
        account("xai-oauth", null, "Grok"),
        account("xai-oauth", "g-2", "Side", { enabled: false }),
        account("chatgpt-codex", null, "Main"),
        account("chatgpt-codex", "c-2", "Team"),
      ];

      it("draws each row's label and state", async () => {
        const { api, shell } = harness();
        api.listProviderAccounts.mockResolvedValue(accounts);
        render(<InferencePanel shell={shell} deployment={rowsDeployment} />);
        const claude = "Anthropic / Claude (subscription)";
        const grok = "Grok (subscription)";
        const chatgpt = "ChatGPT / Codex (subscription)";
        for (const meta of [
          `· ${claude} · Personal · signed in`,
          `· ${claude} · signed in`,
          `· ${grok} · signed in`,
          `· ${grok} · disabled`,
          `· ${chatgpt} · Main · signed in`,
          `· ${chatgpt} · signed in`,
          "· OpenRouter · key stored",
          "· OpenRouter · key from OPENROUTER_API_KEY",
        ])
          expect(await screen.findByText(meta)).toBeVisible();
        expect(screen.getAllByText("· OpenAI compatible · no key")).toHaveLength(2);
        expect(screen.queryByText(/private-credential-id/)).not.toBeInTheDocument();
      });

      it("opens a lapsed token's row as connected, with its label", async () => {
        const { api, shell } = harness();
        api.listProviderAccounts.mockResolvedValue(accounts);
        render(
          <InferencePanel shell={shell} deployment={rowsDeployment} item="claude" />,
        );
        expect(await screen.findByText("Connected")).toBeVisible();
        expect(screen.getByText("Personal")).toBeVisible();
        expect(screen.queryByText("Expired")).not.toBeInTheDocument();
        expect(screen.queryByText("Expires")).not.toBeInTheDocument();
        expect(screen.queryByText(/private-credential-id/)).not.toBeInTheDocument();
      });

      it("offers Add another on every subscription row", async () => {
        const { api, shell } = harness();
        api.listProviderAccounts.mockResolvedValue(accounts);
        api.getInferenceSetupCatalog = vi.fn().mockResolvedValue({
          providers: [
            { id: "openai", displayName: "OpenAI" },
            { id: "anthropic", displayName: "Anthropic" },
            { id: "grok", displayName: "Grok" },
          ],
        });
        render(<InferencePanel shell={shell} deployment={rowsDeployment} />);
        const user = userEvent.setup();
        for (const [row, item] of [
          ["Claude", "Add another Anthropic"],
          ["Side", "Add another Grok"],
          ["Team", "Add another OpenAI"],
        ] as const) {
          await user.click(
            (await screen.findAllByRole("button", { name: `More for ${row}` }))[0]!,
          );
          expect(await screen.findByRole("menuitem", { name: item })).toBeVisible();
          await user.keyboard("{Escape}");
        }
      });

      it("opens a disabled account's row with its identity and Reconnect", async () => {
        const { api, shell } = harness();
        api.listProviderAccounts.mockResolvedValue(accounts);
        render(
          <InferencePanel shell={shell} deployment={rowsDeployment} item="grok-side" />,
        );
        expect(await screen.findByText("Disabled")).toBeVisible();
        expect(screen.getByText("Side-identity")).toBeVisible();
        expect(screen.getByRole("button", { name: "Reconnect" })).toBeVisible();
        expect(
          screen.queryByRole("button", { name: "Disconnect" }),
        ).not.toBeInTheDocument();
      });

      describe("rename, disconnect and remove an account from its row", () => {
        /* Work runs two backends: one a profile uses, one nothing uses */
        const usedDeployment = {
          ...rowsDeployment,
          inferenceBackends: [
            ...rowsDeployment.inferenceBackends,
            row("claude-work-spare", "Work spare", "ClaudeCliSubscription", "acct-2"),
          ],
          inferenceProfiles: [
            {
              ...deployment.inferenceProfiles[0]!,
              profile_id: "writer",
              display_name: "Writer",
              backend_id: "claude-work",
            },
          ],
        };
        const setup = (item?: string) => {
          const { api, shell } = harness();
          api.listProviderAccounts.mockResolvedValue(accounts);
          api.renameProviderAccount = vi.fn().mockResolvedValue(undefined);
          api.removeProviderAccount = vi.fn().mockResolvedValue(undefined);
          render(
            <InferencePanel shell={shell} deployment={usedDeployment} item={item} />,
          );
          return { api, user: userEvent.setup() };
        };
        const openMenu = async (
          user: ReturnType<typeof userEvent.setup>,
          row: string,
        ) =>
          user.click(
            (await screen.findAllByRole("button", { name: `More for ${row}` }))[0]!,
          );
        /* none of the account actions edits a profile or a backend */
        const expectNoConfigWrite = (api: MockApi) => {
          expect(api.patchConfigComponents).not.toHaveBeenCalled();
          expect(api.applyConfigComponents).not.toHaveBeenCalled();
          expect(api.saveInferenceProfileConfig).not.toHaveBeenCalled();
          expect(api.deleteBackendConfig).not.toHaveBeenCalled();
        };

        it("offers the account items on account rows and Remove in place of Delete on an added account's", async () => {
          const { user } = setup();
          await openMenu(user, "Work");
          for (const item of ["Rename account…", "Disconnect…", "Remove account…"])
            expect(await screen.findByRole("menuitem", { name: item })).toBeVisible();
          expect(
            screen.queryByRole("menuitem", { name: "Delete backend…" }),
          ).not.toBeInTheDocument();
          await user.keyboard("{Escape}");

          await openMenu(user, "Claude");
          for (const item of [
            "Rename account…",
            "Disconnect…",
            "Remove account…",
            "Delete backend…",
          ])
            expect(await screen.findByRole("menuitem", { name: item })).toBeVisible();
          await user.keyboard("{Escape}");

          await openMenu(user, "openrouter");
          expect(
            await screen.findByRole("menuitem", { name: "Delete backend…" }),
          ).toBeVisible();
          for (const item of ["Rename account…", "Disconnect…", "Remove account…"])
            expect(
              screen.queryByRole("menuitem", { name: item }),
            ).not.toBeInTheDocument();
        });

        it("renames an account from its row and refuses a label another account shows", async () => {
          const { api, user } = setup();
          await openMenu(user, "Work");
          await user.click(
            await screen.findByRole("menuitem", { name: "Rename account…" }),
          );
          const dialog = await screen.findByRole("dialog");
          await user.clear(within(dialog).getByRole("textbox"));
          await user.type(within(dialog).getByRole("textbox"), "Personal");
          await user.click(within(dialog).getByRole("button", { name: "Save" }));
          expect(await within(dialog).findByRole("alert")).toHaveTextContent(
            "Personal",
          );
          expect(api.renameProviderAccount).not.toHaveBeenCalled();

          await user.clear(within(dialog).getByRole("textbox"));
          await user.type(within(dialog).getByRole("textbox"), "Work 2");
          await user.click(within(dialog).getByRole("button", { name: "Save" }));
          await waitFor(() =>
            expect(api.renameProviderAccount).toHaveBeenCalledTimes(1),
          );
          expect(api.renameProviderAccount).toHaveBeenCalledWith(
            deployment.agentDid,
            "private-credential-id-Work",
            "Work 2",
          );
          await waitFor(() =>
            expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
          );
          expectNoConfigWrite(api);
        });

        it("disconnects an account from its row after naming its profiles", async () => {
          const { api, user } = setup();
          await openMenu(user, "Work");
          await user.click(
            await screen.findByRole("menuitem", { name: "Disconnect…" }),
          );
          let dialog = await screen.findByRole("dialog");
          expect(dialog).toHaveTextContent(
            "These profiles use this account and fail their next turn until moved to another backend: Writer",
          );
          await user.click(
            within(dialog).getByRole("button", { name: "Keep connected" }),
          );
          await waitFor(() =>
            expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
          );
          expect(api.disconnectProviderAccount).not.toHaveBeenCalled();

          await openMenu(user, "Work");
          await user.click(
            await screen.findByRole("menuitem", { name: "Disconnect…" }),
          );
          dialog = await screen.findByRole("dialog");
          await user.click(
            within(dialog).getByRole("button", { name: "Disconnect now" }),
          );
          await waitFor(() =>
            expect(api.disconnectProviderAccount).toHaveBeenCalledTimes(1),
          );
          expect(api.disconnectProviderAccount).toHaveBeenCalledWith(
            deployment.agentDid,
            "private-credential-id-Work",
          );
          expectNoConfigWrite(api);
        });

        it("removes an account from its row after naming its profiles and backends", async () => {
          const { api, user } = setup();
          await openMenu(user, "Work");
          await user.click(
            await screen.findByRole("menuitem", { name: "Remove account…" }),
          );
          const dialog = await screen.findByRole("alertdialog");
          expect(dialog).toHaveTextContent(
            "These profiles use this account and fail their next turn until moved to another backend: Writer",
          );
          expect(dialog).toHaveTextContent("Deletes its unused backends: Work spare");
          expect(dialog).toHaveTextContent("Keeps the backends a profile uses: Work");
          const remove = within(dialog).getByRole("button", { name: /^Delete / });
          expect(remove).toBeDisabled();
          await user.type(within(dialog).getByRole("textbox"), "Work");
          await user.click(remove);
          await waitFor(() =>
            expect(api.removeProviderAccount).toHaveBeenCalledTimes(1),
          );
          expect(api.removeProviderAccount).toHaveBeenCalledWith(
            deployment.agentDid,
            "private-credential-id-Work",
          );
          expectNoConfigWrite(api);
        });

        it("offers Remove account, not Delete backend, in an added account's Danger zone", async () => {
          const { user } = setup("claude-work");
          await screen.findByText("Work-identity");
          const zone = screen.getByTestId("danger-zone");
          expect(within(zone).queryByText(/Delete backend/)).not.toBeInTheDocument();
          await user.click(within(zone).getByRole("button", { name: /account/ }));
          expect(await screen.findByRole("alertdialog")).toHaveTextContent(
            "Deletes its unused backends: Work spare",
          );
        });
      });

      describe("rows draw reported usage only, read on open and on Refresh", () => {
        const at = (minutes: number) =>
          new Date(Date.now() + minutes * 60_000).toISOString();
        const window = (label: string, usedPct: number, fields: object = {}) => ({
          label,
          windowMinutes: null,
          usedPct,
          resetsAt: at(133),
          source: "header",
          observedAt: at(-3),
          lastKnown: false,
          ...fields,
        });
        const view_ = (backendId: string, fields: object = {}) => ({
          backendId,
          windows: [],
          plan: null,
          note: null,
          readAt: null,
          readError: null,
          read: null,
          ...fields,
        });
        const usage = [
          view_("claude-work", { windows: [window("5h", 30), window("7d", 81)] }),
          view_("chatgpt-team", {
            windows: [window("5h", 20, { observedAt: at(-70), lastKnown: true })],
          }),
          view_("chatgpt", { note: "unknown" }),
          view_("local", { note: "not reported" }),
          view_("openrouter", {
            note: "no cap on this key",
            read: "unavailable: throttled",
          }),
          view_("claude", { note: "not verified", read: "skipped_until_refresh" }),
          view_("grok-side", { windows: [window("5h", 50)] }),
        ];
        const setup = (item?: string) => {
          const { api, shell } = harness();
          api.listProviderAccounts.mockResolvedValue(accounts);
          api.readProviderUsage = vi.fn().mockResolvedValue(usage);
          const view = render(
            <InferencePanel shell={shell} deployment={rowsDeployment} item={item} />,
          );
          return { api, shell, view, user: userEvent.setup() };
        };

        it("reads usage once on open and not on a snapshot change", async () => {
          const { api, shell, view } = setup();
          await waitFor(() => expect(api.readProviderUsage).toHaveBeenCalledTimes(1));
          expect(api.readProviderUsage).toHaveBeenCalledWith(
            deployment.agentDid,
            false,
            null,
          );
          view.rerender(
            <InferencePanel
              shell={{ ...shell, snapshot: { bootstrap } } as Shell}
              deployment={rowsDeployment}
            />,
          );
          await act(async () => {});
          expect(api.readProviderUsage).toHaveBeenCalledTimes(1);
        });

        it("draws the most-used window on a row and no percent without a window", async () => {
          setup();
          expect(await screen.findByText("7d 81%")).toBeVisible();
          expect(screen.getByText("5h 20%")).toBeVisible();
          /* the disabled Side account draws no usage; no other row has a window */
          expect(screen.getAllByText(/\d+%$/).map((e) => e.textContent)).toEqual([
            "7d 81%",
            "5h 20%",
          ]);
        });

        it("opens a row with each window's percent, reset, source and age", async () => {
          setup("claude-work");
          for (const pct of [30, 81])
            expect(
              await screen.findByText(
                new RegExp(
                  `^${pct}% used · resets in 2h1[23]m \\(.+\\) · from response headers, [23]m ago$`,
                ),
              ),
            ).toBeVisible();
        });

        it("marks a stale window as last known", async () => {
          setup("chatgpt-team");
          expect(
            await screen.findByText(
              /^20% used · .* · from response headers, 1h1[01]m ago · last known$/,
            ),
          ).toBeVisible();
        });

        for (const [item, texts] of [
          ["chatgpt", ["unknown"]],
          ["local", ["not reported"]],
          ["openrouter", ["no cap on this key", "Not read: throttled"]],
          ["claude", ["not verified", "Refresh to read usage"]],
        ] as const)
          it(`says why ${item} has no number`, async () => {
            setup(item);
            for (const text of texts)
              expect(await screen.findByText(text)).toBeVisible();
            expect(screen.queryByText(/% used/)).not.toBeInTheDocument();
          });

        it("draws no usage for a disabled account", async () => {
          setup("grok-side");
          expect(await screen.findByText("Disabled")).toBeVisible();
          expect(screen.queryByText(/50%/)).not.toBeInTheDocument();
          expect(
            screen.queryByRole("button", { name: "Refresh" }),
          ).not.toBeInTheDocument();
        });

        it("Refresh reads the row's provider and redraws from the result", async () => {
          const { api, user, view } = setup("claude");
          await screen.findByText("not verified");
          api.readProviderUsage.mockResolvedValueOnce([
            view_("claude", { windows: [window("5h", 12)] }),
          ]);
          await user.click(screen.getByRole("button", { name: "Refresh" }));
          expect(api.readProviderUsage).toHaveBeenLastCalledWith(
            deployment.agentDid,
            true,
            "claude-subscription",
          );
          expect(await screen.findByText(/^12% used/)).toBeVisible();
          view.unmount();

          const openrouter = setup("openrouter");
          await screen.findByText("no cap on this key");
          await openrouter.user.click(screen.getByRole("button", { name: "Refresh" }));
          expect(openrouter.api.readProviderUsage).toHaveBeenLastCalledWith(
            deployment.agentDid,
            true,
            null,
          );
        });
      });

      describe("profiles pick accounts by provider and label", () => {
        const pickDeployment = {
          ...rowsDeployment,
          inferenceBackends: [
            ...rowsDeployment.inferenceBackends,
            row("claude-gone", "Gone", "ClaudeCliSubscription", "acct-gone"),
          ],
        };
        const editProfileOn = (backendId: string) => {
          const { api, shell } = harness();
          api.listProviderAccounts.mockResolvedValue(accounts);
          render(
            <ProfileEditor
              shell={shell}
              deployment={pickDeployment}
              profile={{ ...deployment.inferenceProfiles[0]!, backend_id: backendId }}
            />,
          );
          return userEvent.setup();
        };

        it("the profile backend field names accounts and skips unusable ones", async () => {
          const user = editProfileOn("openrouter");
          await user.click(screen.getByRole("combobox", { name: "Backend" }));
          expect(
            await screen.findByRole("option", { name: /^Work\s*Anthropic \/ Claude$/ }),
          ).toBeVisible();
          expect(
            screen.getByRole("option", { name: /^Personal\s*Anthropic \/ Claude$/ }),
          ).toBeVisible();
          expect(
            screen.getByRole("option", { name: /^Grok\s*Grok \/ xAI$/ }),
          ).toBeVisible();
          /* Side is disabled; Gone's account is not on this node */
          expect(
            screen.queryByRole("option", { name: /^Side/ }),
          ).not.toBeInTheDocument();
          expect(
            screen.queryByRole("option", { name: /^Gone/ }),
          ).not.toBeInTheDocument();
        });

        it("the profile backend field keeps an unusable backend that is the current one", async () => {
          const user = editProfileOn("grok-side");
          await user.click(screen.getByRole("combobox", { name: "Backend" }));
          await screen.findByRole("option", { name: /^Work/ });
          expect(screen.getByRole("option", { name: /^Side/ })).toBeVisible();
          expect(
            screen.queryByRole("option", { name: /^Gone/ }),
          ).not.toBeInTheDocument();
        });

        const personalOff = accounts.map((a) =>
          a.label === "Personal" ? { ...a, enabled: false } : a,
        );
        it("new profile preselects the first enabled account, skipping a disabled first one", () => {
          expect(
            newProfileDocument(rowsDeployment, undefined, personalOff).backend_id,
          ).toBe("claude-work");
        });

        it("new profile preselects the asked backend when its account is usable", () => {
          expect(
            newProfileDocument(rowsDeployment, "claude-work", personalOff).backend_id,
          ).toBe("claude-work");
        });

        it("new profile preselects another backend when the asked one's account is disabled", () => {
          expect(
            newProfileDocument(rowsDeployment, "claude", personalOff).backend_id,
          ).toBe("claude-work");
        });

        it("new profile preselects the provider's first account in resolver order", () => {
          const workFirst = [accounts[1]!, accounts[0]!, ...accounts.slice(2)];
          expect(
            newProfileDocument(rowsDeployment, undefined, workFirst).backend_id,
          ).toBe("claude-work");
        });
      });
    });
  });

  it("does not treat a disabled subscription credential as signed in", async () => {
    const { api, shell } = harness();
    api.listProviderAccounts.mockResolvedValue([
      {
        provider: "xai-oauth",
        enabled: false,
        credentialId: "disabled-credential",
      },
    ]);
    render(
      <InferencePanel
        shell={shell}
        deployment={{
          ...deployment,
          inferenceBackends: [
            {
              ...deployment.inferenceBackends[0]!,
              providerKind: "XaiGrokOAuth",
            },
          ],
        }}
      />,
    );
    expect(await screen.findByText(/disabled/)).toBeVisible();
  });

  it("shows runtime execution defaults and backend model choices without expanding advanced settings", async () => {
    const { api, shell } = harness();
    api.getInferenceSetupCatalog = vi.fn().mockResolvedValue({
      executionDefaults: {
        maxTurns: 250,
        maxTotalTokens: null,
        streamBatchMs: 100,
        streamLivenessSecs: 1800,
        providerIdleSecs: 300,
        deadlineSecs: 86400,
      },
    });
    render(<ProfilesPanel shell={shell} deployment={deployment} item="profile-a" />);
    await waitFor(() => expect(screen.getByLabelText("Max turns")).toHaveValue("250"));
    await replace("Max turns", "200");
    expect(screen.getByLabelText("Max turns")).toHaveValue("200");
    expect(screen.getByRole("combobox", { name: "Model" })).toBeVisible();
    expect(
      screen.queryByRole("button", { name: "Advanced settings" }),
    ).not.toBeInTheDocument();
  });

  it("keeps a profile whose backend is gone on the Providers page", async () => {
    const { api, shell } = harness();
    api.getInferenceSetupCatalog = vi.fn().mockResolvedValue({ providers: [] });
    const orphan = {
      ...deployment.inferenceProfiles[0]!,
      profile_id: "profile-orphan",
      display_name: "Orphaned profile",
      backend_id: "backend-deleted",
    };
    render(
      <ProfilesPanel
        shell={shell}
        deployment={{
          ...deployment,
          inferenceProfiles: [...deployment.inferenceProfiles, orphan],
        }}
      />,
    );
    expect(await screen.findByText("Orphaned profile")).toBeInTheDocument();
    expect(screen.getAllByText("Backend is missing").length).toBeGreaterThan(0);
  });

  it("says when the provider catalog cannot be read and reads it again on Retry", async () => {
    const { api, shell } = harness();
    api.getInferenceSetupCatalog = vi
      .fn()
      .mockRejectedValueOnce(new Error("catalog offline"))
      .mockResolvedValueOnce({ providers: [] });
    render(<InferencePanel shell={shell} deployment={deployment} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("catalog offline");
    await userEvent.setup().click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(screen.queryByRole("alert")).not.toBeInTheDocument());
    expect(api.getInferenceSetupCatalog).toHaveBeenCalledTimes(2);
  });

  it("opens shared provider setup without eagerly creating a blank backend", async () => {
    const { api, shell } = harness();
    api.getInferenceSetupCatalog = vi.fn().mockResolvedValue({
      providers: [
        {
          id: "openai",
          displayName: "OpenAI",
          description: "OpenAI models",
          authMethods: ["api_key"],
          authOptions: [
            {
              method: "api_key",
              displayName: "API key",
              defaultEndpoint: "https://api.openai.com/v1",
            },
          ],
          defaultAuthMethod: "api_key",
          defaultEndpoint: "https://api.openai.com/v1",
        },
      ],
    });
    render(<InferencePanel shell={shell} deployment={deployment} />);
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "New backend" }));
    await user.click(await screen.findByRole("menuitem", { name: "OpenAI" }));
    expect(await screen.findByRole("heading", { name: "Set up OpenAI" })).toBeVisible();
    expect(api.getInferenceSetupCatalog).toHaveBeenCalled();
    expect(api.saveBackendConfig).not.toHaveBeenCalled();
    expect(api.applyConfigComponents).not.toHaveBeenCalled();
  });

  it("requires the agent identity fields and saves editable principal tags", async () => {
    const { api, shell } = harness();
    render(<AgentPanel shell={shell} deployment={deployment} />);
    expectFields(["Display name", "Default behavior", "Enabled", "Tags"]);

    const user = await replace("Display name", " ");
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Display name is required",
    );
    expect(api.saveAgentConfig).not.toHaveBeenCalled();

    await replace("Display name", "Acceptance Agent");
    await replace("Tags", "acceptance{Enter}desktop{Enter}");
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(api.saveAgentConfig).toHaveBeenCalledWith(
      expect.objectContaining({
        document: expect.objectContaining({
          display_name: "Acceptance Agent",
          tags: ["acceptance", "desktop"],
        }),
      }),
    );
  });

  it("validates and saves every behavior-owned setting without changing Setup", async () => {
    const { api, shell } = harness();
    render(<BehaviorsPanel shell={shell} deployment={deployment} behaviorId="ops" />);
    expectFields([
      "Display name",
      "Description",
      "System prompt",
      "Inference profile",
      "Tags",
    ]);
    const user = await replace("Display name", " ");
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Give the behavior a name.",
    );
    expect(api.saveBehaviorConfig).not.toHaveBeenCalled();
  });

  it("opens a new behavior as an unsaved, disabled draft until the operator saves it", async () => {
    const { api, shell } = harness();
    render(<BehaviorsPanel shell={shell} deployment={deployment} />);
    const user = userEvent.setup();

    await user.click(screen.getByRole("button", { name: "New behavior" }));

    expect(screen.getByLabelText("Display name")).toHaveValue("");
    expect(api.saveBehaviorConfig).not.toHaveBeenCalled();
    expect(api.applyConfigComponents).not.toHaveBeenCalled();

    await user.type(screen.getByLabelText("Display name"), "Reviewer");
    await user.click(screen.getByRole("button", { name: "Create" }));
    /* the new context and the behavior that points at it are one apply */
    await waitFor(() => expect(api.applyConfigComponents).toHaveBeenCalledTimes(1));
    const request = api.applyConfigComponents.mock.calls[0][0];
    expect(request.document.agent_behaviors).toEqual([
      expect.objectContaining({
        display_name: "Reviewer",
        enabled: false,
        context_id: request.document.contexts[0].context_id,
      }),
    ]);
    expect(api.saveBehaviorConfig).not.toHaveBeenCalled();
  });

  it("keeps a new behavior's draft open on a failed save and retries the same context", async () => {
    const { api, shell } = harness();
    api.applyConfigComponents
      .mockRejectedValueOnce(new Error("bridge offline"))
      .mockResolvedValueOnce({});
    const onSaved = vi.fn();
    render(
      <BehaviorEditor
        shell={shell}
        deployment={deployment}
        behavior={newBehaviorView(deployment)}
        draft={{ onSaved, onCancel: vi.fn() }}
      />,
    );
    const user = userEvent.setup();
    await user.type(screen.getByLabelText("Display name"), "Reviewer");
    await user.click(screen.getByRole("button", { name: "Create" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("bridge offline");
    expect(onSaved).not.toHaveBeenCalled();
    expect(screen.getByLabelText("Display name")).toHaveValue("Reviewer");

    await user.click(screen.getByRole("button", { name: "Create" }));
    await waitFor(() => expect(onSaved).toHaveBeenCalledTimes(1));
    const [first, second] = api.applyConfigComponents.mock.calls.map(
      (call) => call[0].document.contexts[0].context_id,
    );
    expect(second).toBe(first);
  });

  it("creates a prefilled new profile as it stands, and closes only on success", async () => {
    const { api, shell } = harness();
    api.applyConfigComponents
      .mockRejectedValueOnce(new Error("write refused"))
      .mockResolvedValueOnce({});
    const onSaved = vi.fn();
    /* a prefilled draft, as Add profile under a backend opens it */
    const profile = {
      ...newProfileDocument(deployment, "backend-a"),
      model_name: deployment.inferenceProfiles[0]!.model_name,
      backend_id: deployment.inferenceProfiles[0]!.backend_id,
    };
    render(
      <ProfileEditor
        shell={shell}
        deployment={deployment}
        profile={profile}
        draft={{ onSaved, onCancel: vi.fn() }}
        embedded
      />,
    );
    const user = userEvent.setup();
    /* model-aware defaults arrive before a profile can be written */
    await waitFor(() =>
      expect(api.getInferenceBackendRecommendation).toHaveBeenCalled(),
    );
    await act(async () => {
      await Promise.resolve();
    });
    const create = screen.getByRole("button", { name: "Create" });
    expect(create).toBeEnabled();

    await user.click(create);
    expect(await screen.findByRole("alert")).toHaveTextContent("write refused");
    expect(onSaved).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "Create" }));
    await waitFor(() => expect(onSaved).toHaveBeenCalledWith(profile.profile_id));
    expect(api.applyConfigComponents).toHaveBeenCalledTimes(2);
    expect(
      api.applyConfigComponents.mock.calls[1][0].document.inference_profiles,
    ).toEqual([expect.objectContaining({ profile_id: profile.profile_id })]);
  });

  it("turns a behavior on or off from its row with a patch of enabled alone", async () => {
    const { api, shell } = harness();
    render(<BehaviorsPanel shell={shell} deployment={deployment} />);
    const ops = deployment.behaviors.find((b) => b.behaviorId === "ops")!;
    const toggle = screen.getAllByRole("switch", {
      name: `${ops.displayName} is ${ops.enabled ? "enabled" : "disabled"}`,
    })[0]!;
    await userEvent.setup().click(toggle);
    await waitFor(() =>
      expect(api.patchConfigComponents).toHaveBeenCalledWith({
        agentDid: deployment.agentDid,
        patches: [
          {
            collection: "AgentBehavior",
            id: "ops",
            changes: { enabled: !ops.enabled },
          },
        ],
      }),
    );
    expect(api.saveBehaviorConfig).not.toHaveBeenCalled();
  });

  it("makes a behavior the agent's default from its row menu", async () => {
    const { api, shell } = harness();
    render(<BehaviorsPanel shell={shell} deployment={deployment} />);
    const user = userEvent.setup();
    const ops = deployment.behaviors.find((b) => b.behaviorId === "ops")!;
    await user.click(
      screen.getAllByRole("button", { name: `More for ${ops.displayName}` })[0]!,
    );
    await user.click(await screen.findByRole("menuitem", { name: "Make default" }));
    await waitFor(() =>
      expect(api.setDefaultBehavior).toHaveBeenCalledWith({
        agentDid: deployment.agentDid,
        behaviorId: "ops",
      }),
    );
    expect(api.saveAgentConfig).not.toHaveBeenCalled();
  });

  describe("a default behavior is enabled", () => {
    const withOps = (changes: Partial<(typeof deployment.behaviors)[number]>) => ({
      ...deployment,
      behaviors: deployment.behaviors.map((b) =>
        b.behaviorId === "ops" ? { ...b, ...changes } : b,
      ),
    });

    it("makes a disabled behavior the default and enables it in one call", async () => {
      const { api, shell } = harness();
      render(<BehaviorsPanel shell={shell} deployment={withOps({ enabled: false })} />);
      const user = userEvent.setup();
      await user.click(screen.getAllByRole("button", { name: "More for Ops" })[0]!);
      const item = await screen.findByRole("menuitem", { name: /^Make default/ });
      expect(item).toHaveTextContent("Also enables it");
      await user.click(item);
      await waitFor(() =>
        expect(api.setDefaultBehavior).toHaveBeenCalledWith({
          agentDid: deployment.agentDid,
          behaviorId: "ops",
        }),
      );
      expect(api.patchConfigComponents).not.toHaveBeenCalled();
      expect(api.saveAgentConfig).not.toHaveBeenCalled();
    });

    it("offers Default to a disabled behavior with no instructions", async () => {
      const { api, shell } = harness();
      render(
        <BehaviorsPanel
          shell={shell}
          deployment={withOps({ enabled: false, contextId: null })}
        />,
      );
      const user = userEvent.setup();
      await user.click(screen.getAllByRole("button", { name: "More for Ops" })[0]!);
      const item = await screen.findByRole("menuitem", { name: /^Make default/ });
      expect(item).not.toHaveAttribute("aria-disabled");
      await user.click(item);
      await waitFor(() =>
        expect(api.setDefaultBehavior).toHaveBeenCalledWith({
          agentDid: deployment.agentDid,
          behaviorId: "ops",
        }),
      );
    });

    it("refuses to turn off the current default and explains why", async () => {
      const { api, shell } = harness();
      render(<BehaviorsPanel shell={shell} deployment={deployment} />);
      const toggle = screen.getAllByRole("switch", { name: "Default is enabled" })[0]!;
      expect(toggle).toHaveAttribute("aria-disabled", "true");
      expect(toggle).toHaveAttribute(
        "aria-description",
        "The default behavior stays enabled. Choose another default before turning it off.",
      );
      await userEvent.setup().click(toggle);
      expect(api.patchConfigComponents).not.toHaveBeenCalled();
      /* another behavior still turns off */
      expect(
        screen.getAllByRole("switch", { name: "Ops is enabled" })[0],
      ).toBeEnabled();
    });

    it("sets a new agent default in one call before saving the other fields", async () => {
      const { api, shell } = harness();
      render(<AgentPanel shell={shell} deployment={withOps({ enabled: false })} />);
      const user = userEvent.setup();
      await user.click(screen.getByRole("combobox", { name: "Default behavior" }));
      await user.click(await screen.findByRole("option", { name: /^Ops/ }));
      await user.click(screen.getByRole("button", { name: "Save" }));
      await waitFor(() => expect(api.saveAgentConfig).toHaveBeenCalled());
      expect(api.setDefaultBehavior).toHaveBeenCalledWith({
        agentDid: deployment.agentDid,
        behaviorId: "ops",
      });
      expect(api.setDefaultBehavior.mock.invocationCallOrder[0]).toBeLessThan(
        api.saveAgentConfig.mock.invocationCallOrder[0]!,
      );
      expect(api.patchConfigComponents).not.toHaveBeenCalled();
    });

    it("shows the publication refusal when a default cannot be set", async () => {
      const { api, shell } = harness();
      api.setDefaultBehavior.mockRejectedValue(
        new Error(
          'AgentBehavior ops field context_id references missing AgentContext "gone"',
        ),
      );
      render(<AgentPanel shell={shell} deployment={withOps({ enabled: false })} />);
      const user = userEvent.setup();
      await user.click(screen.getByRole("combobox", { name: "Default behavior" }));
      await user.click(await screen.findByRole("option", { name: /^Ops/ }));
      await user.click(screen.getByRole("button", { name: "Save" }));
      expect(await screen.findByRole("alert")).toHaveTextContent(
        "references missing AgentContext",
      );
      expect(api.saveAgentConfig).not.toHaveBeenCalled();
    });
  });

  it("patches only the changed context field and the behavior in one call", async () => {
    const { api, shell } = harness();
    render(<BehaviorsPanel shell={shell} deployment={deployment} behaviorId="ops" />);
    const user = userEvent.setup();
    const prompt = screen.getByLabelText("System prompt");
    await user.clear(prompt);
    await user.type(prompt, "Watch the fleet");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(api.patchConfigComponents).toHaveBeenCalledTimes(1));
    const { patches } = api.patchConfigComponents.mock.calls[0][0];
    expect(patches[0]).toEqual({
      collection: "AgentContext",
      id: "context-b",
      changes: { system_prompt: "Watch the fleet" },
    });
    expect(patches[1]).toEqual(
      expect.objectContaining({ collection: "AgentBehavior", id: "ops" }),
    );
    expect(api.applyConfigComponents).not.toHaveBeenCalled();
    expect(api.saveBehaviorConfig).not.toHaveBeenCalled();
  });

  it("says when the runtime's execution defaults cannot be read", async () => {
    const { api, shell } = harness();
    api.getInferenceSetupCatalog = vi.fn().mockRejectedValue(new Error("no catalog"));
    render(<ProfilesPanel shell={shell} deployment={deployment} item="profile-a" />);
    expect(
      await screen.findByText(
        /Couldn’t read the runtime’s execution defaults: no catalog/,
      ),
    ).toBeInTheDocument();
  });

  it("creates an automation off when the bridge says its behavior cannot run", async () => {
    const { shell } = harness();
    const blocked = {
      ...deployment,
      behaviorReadiness: {
        ...deployment.behaviorReadiness,
        behaviors: [
          {
            state: "unavailable",
            behaviorId: "default",
            reason: "credentials_required",
          },
          { state: "ready", behaviorId: "ops" },
        ],
      },
    } as typeof deployment;
    render(<TasksPanel shell={shell} deployment={blocked} item="task-a" />);
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Schedule" }));
    const dialog = await screen.findByRole("dialog", { name: "When it runs" });
    expect(
      within(dialog).getByRole("checkbox", { name: "Turn it on now" }),
    ).toHaveAttribute("aria-disabled", "true");
  });

  it("coalesces repeated create activation while the operator write is pending", async () => {
    const { api, shell } = harness();
    let finish: (() => void) | undefined;
    api.saveSkillConfig.mockImplementation(
      () =>
        new Promise<void>((resolve) => {
          finish = resolve;
        }),
    );
    render(<SkillsPanel shell={shell} deployment={deployment} />);
    const button = screen.getByRole("button", { name: "New skill" });

    act(() => {
      button.click();
      button.click();
    });

    expect(api.saveSkillConfig).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("button", { name: "Creating…" })).toBeDisabled();
    finish?.();
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "New skill" })).toBeEnabled(),
    );
  });

  it("uses the real context delete command and never a replacement-list apply", async () => {
    const { api, shell } = harness();
    render(<ContextsPanel shell={shell} deployment={deployment} item="context-b" />);
    expectFields([
      "Display name",
      "Description",
      "System prompt",
      "Tools",
      "Compaction",
      "Tags",
    ]);
    expect(screen.getByRole("combobox", { name: "Skills" })).toBeInTheDocument();
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Delete context" }));
    expect(api.deleteContextConfig).not.toHaveBeenCalled();
    expect(
      screen.getByText(/Ops uses it and will be left without a context/),
    ).toBeInTheDocument();
    const confirm = screen.getByRole("textbox", {
      name: "Type Ops context to confirm",
    });
    await user.type(confirm, "wrong");
    expect(screen.getByRole("button", { name: "Delete context" })).toBeDisabled();
    await user.clear(confirm);
    await user.type(confirm, "Ops context");
    await user.click(screen.getByRole("button", { name: "Delete context" }));
    expect(api.deleteContextConfig).toHaveBeenCalledWith({
      contextId: "context-b",
      agentDid: deployment.agentDid,
    });
    expect(api.applyConfigComponents).not.toHaveBeenCalled();
  });

  it("creates only the new behavior's context and represents empty lists as null", async () => {
    const { api, shell } = harness();
    render(<BehaviorsPanel shell={shell} deployment={deployment} />);
    const user = userEvent.setup();

    await user.click(screen.getByRole("button", { name: "New behavior" }));
    await user.type(screen.getByLabelText("Display name"), "Reviewer");
    await user.click(screen.getByRole("button", { name: "Create" }));

    await waitFor(() => expect(api.applyConfigComponents).toHaveBeenCalledTimes(1));
    const request = api.applyConfigComponents.mock.calls[0][0];
    expect(request.document.contexts).toHaveLength(1);
    expect(request.document.contexts[0]).toEqual(
      expect.objectContaining({ skill_ids: null, tags: null }),
    );
  });

  it("rejects invalid backend endpoints and capacity before writing", async () => {
    const { api, shell } = harness();
    render(<InferencePanel shell={shell} deployment={deployment} item="backend-a" />);
    expectFields([
      "Name",
      "Provider kind",
      "OpenAI wire API",
      "API key env var",
      "API key",
      "Endpoint",
      "Connect timeout seconds",
      "Discovery timeout seconds",
      "Max concurrent",
      "Max queue depth",
      "Enabled",
      "Tags",
    ]);
    const user = await replace("Endpoint", "file:///tmp/model");
    await replace("Max concurrent", "0");
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Endpoint must use http or https",
    );
    expect(api.patchConfigComponents).not.toHaveBeenCalled();
  });

  it("requires an inline second action before disconnecting a subscription", async () => {
    const { api, shell } = harness();
    api.listProviderAccounts.mockResolvedValue([
      {
        credentialId: "credential-a",
        agentDid: deployment.agentDid,
        provider: "xai-oauth",
        accountId: "account-a",
        planType: "supergrok",
        accessTokenExpiresAt: "2099-01-01T00:00:00Z",
        lastRefresh: null,
        enabled: true,
      },
    ]);
    const subscriptionDeployment = {
      ...deployment,
      inferenceBackends: deployment.inferenceBackends.map((backend) =>
        backend.backendId === "backend-a"
          ? { ...backend, providerKind: "XaiGrokOAuth" as const }
          : backend,
      ),
    };
    render(
      <InferencePanel
        shell={shell}
        deployment={subscriptionDeployment}
        item="backend-a"
      />,
    );

    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Disconnect" }));
    expect(api.disconnectProviderAccount).not.toHaveBeenCalled();
    expect(screen.getByText("Disconnect Grok / xAI?")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Keep connected" }));
    expect(api.disconnectProviderAccount).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "Disconnect" }));
    await user.click(screen.getByRole("button", { name: "Disconnect now" }));
    expect(api.disconnectProviderAccount).toHaveBeenCalledWith(
      deployment.agentDid,
      "credential-a",
    );
  });

  it("validates model-supported profile defaults and execution settings", async () => {
    const { api, shell } = harness();
    render(<ProfilesPanel shell={shell} deployment={deployment} item="profile-a" />);
    await screen.findByLabelText("Temperature");
    expect(screen.queryByText("Gents recommends balanced sampling.")).toBeNull();
    const user = userEvent.setup();
    expect(screen.queryByRole("button", { name: "Advanced settings" })).toBeNull();
    await user.click(screen.getByText("Description", { selector: "summary" }));
    expectFields([
      "Display name",
      "Description",
      "Backend",
      "Model",
      "Execution document ID",
      "Max turns",
      "Max total tokens",
      "Stream batch ms",
      "Execution lease seconds",
      "Provider idle seconds",
      "Deadline seconds",
      "Retry policy ID",
      "Tags",
    ]);
    expect(screen.getByLabelText("Temperature")).toHaveValue(0.7);
    expect(screen.getByLabelText("Top-p")).toHaveValue(1);
    const topP = screen.getByLabelText("Top-p");
    await user.clear(topP);
    await user.type(topP, "1.5");
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Top-p must be between 0 and 1",
    );
    expect(api.applyConfigComponents).not.toHaveBeenCalled();
  });

  it("rejects relative roots and malformed advanced tool configuration", async () => {
    const { api, shell } = harness();
    render(<ToolsPanel shell={shell} deployment={deployment} item="tools-a" />);
    expectFields([
      "Display name",
      "Workspace root",
      "Files",
      "Bash",
      "Background processes",
      "Canonical JSON",
    ]);
    const user = await replace("Workspace root", "relative/repo");
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Workspace root must be an absolute path",
    );
    expect(api.saveToolsConfig).not.toHaveBeenCalled();
  });

  it("validates and tests the complete MCP service address", async () => {
    const { api, shell } = harness();
    render(
      <ToolServicesPanel shell={shell} deployment={deployment} item="service-a" />,
    );
    expectFields([
      "Display name",
      "Description",
      "Hostname",
      "Tailscale IP",
      "LAN IP",
      "MCP port",
      "MCP path",
      "Send agent DID",
      "Enabled",
      "Tags",
    ]);
    const user = await replace("MCP port", "70000");
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "MCP port must be 65535 or less",
    );
    expect(api.saveToolServiceConfig).not.toHaveBeenCalled();

    await replace("MCP port", "7331");
    await user.click(screen.getByRole("button", { name: "Test connection" }));
    expect(api.testToolService).toHaveBeenCalledWith(
      expect.objectContaining({ mcpPort: 7331, mcpPath: "/mcp" }),
    );
  });

  it("preserves and validates skill interface metadata", async () => {
    const { api, shell } = harness();
    render(<SkillsPanel shell={shell} deployment={deployment} item="skill-a" />);
    expectFields([
      "Name",
      "Display name",
      "Enabled",
      "Description",
      "Instructions",
      "Tool dependencies",
      "Interface JSON",
      "Tags",
    ]);
    const user = await replace("Interface JSON", "not-json");
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Interface JSON must be valid JSON",
    );
    expect(api.saveSkillConfig).not.toHaveBeenCalled();
  });

  it("validates task prompts, goal budgets, hooks, and manual-run args", async () => {
    const { api, shell } = harness();
    render(<TasksPanel shell={shell} deployment={deployment} item="task-a" />);
    expectFields([
      "Name",
      "Behavior",
      "Enabled",
      "Description",
      "Prompt template",
      "Durable goal objective",
      "Goal token budget",
      "Output schema ref",
      "Tags",
      "Args",
    ]);
    expect(screen.getByText("Hooks")).toBeInTheDocument();
    const user = await replace("Prompt template", " ");
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Prompt template is required",
    );
    expect(api.saveTaskConfig).not.toHaveBeenCalled();
  });

  it("adds a schedule to a task only on Create, as one apply with the trigger off", async () => {
    const { api, shell } = harness();
    api.applyConfigComponents
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValueOnce({});
    render(<TasksPanel shell={shell} deployment={deployment} item="task-a" />);
    const user = userEvent.setup();

    await user.click(screen.getByRole("button", { name: "Schedule" }));
    const dialog = await screen.findByRole("dialog", { name: "When it runs" });
    expect(api.applyConfigComponents).not.toHaveBeenCalled();
    expect(api.saveScheduleConfig).not.toHaveBeenCalled();
    expect(api.saveTriggerConfig).not.toHaveBeenCalled();

    await user.click(within(dialog).getByRole("button", { name: "Create" }));
    expect(await within(dialog).findByText("offline")).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Create" }));
    await waitFor(() => expect(api.applyConfigComponents).toHaveBeenCalledTimes(2));

    const [first, second] = api.applyConfigComponents.mock.calls.map(
      (call) => call[0].document,
    );
    expect(second.schedules).toHaveLength(1);
    expect(second.triggers).toEqual([
      expect.objectContaining({
        task_id: "task-a",
        enabled: false,
        source: { kind: "schedule", schedule_id: second.schedules[0].schedule_id },
      }),
    ]);
    expect(second.tasks).toBeUndefined();
    /* a retry writes the same documents, never a second copy */
    expect(second.schedules[0].schedule_id).toBe(first.schedules[0].schedule_id);
    expect(second.triggers[0].trigger_id).toBe(first.triggers[0].trigger_id);
    expect(api.saveScheduleConfig).not.toHaveBeenCalled();
    expect(api.saveTriggerConfig).not.toHaveBeenCalled();
  });

  it("asks which collection an event watches instead of defaulting to requests", async () => {
    const { api, shell } = harness();
    render(<TasksPanel shell={shell} deployment={deployment} item="task-a" />);
    const user = userEvent.setup();

    await user.click(screen.getByRole("button", { name: "Event" }));
    const dialog = await screen.findByRole("dialog", { name: "When it runs" });
    expect(within(dialog).getByLabelText("Collection")).toHaveValue("");
    await user.click(within(dialog).getByRole("button", { name: "Create" }));
    expect(
      await within(dialog).findByText("Name the collection to watch."),
    ).toBeInTheDocument();
    expect(api.applyConfigComponents).not.toHaveBeenCalled();
    expect(api.saveEventSourceConfig).not.toHaveBeenCalled();

    await user.type(within(dialog).getByLabelText("Collection"), "MailboxItem");
    const turnOn = within(dialog).getByRole("checkbox", { name: "Turn it on now" });
    expect(turnOn).toHaveAttribute("aria-checked", "false");
    await user.click(within(dialog).getByText("Turn it on now"));
    expect(turnOn).toHaveAttribute("aria-checked", "true");
    await user.click(within(dialog).getByRole("button", { name: "Create" }));
    await waitFor(() => expect(api.applyConfigComponents).toHaveBeenCalledTimes(1));
    const document = api.applyConfigComponents.mock.calls[0][0].document;
    expect(document.event_sources).toEqual([
      expect.objectContaining({
        source_collection: "MailboxItem",
        event_kind: "created",
      }),
    ]);
    expect(document.triggers[0].enabled).toBe(true);
  });

  it("does not publish a task result after the user changes compose intent", async () => {
    const { shell } = harness();
    let generation = 0;
    let resolve!: (result: { requestId: string; sessionId: string }) => void;
    const pending = new Promise<{ requestId: string; sessionId: string }>((next) => {
      resolve = next;
    });
    Object.assign(shell, {
      runTask: vi.fn(() => pending),
      captureComposeIntent: () => generation,
      acceptsComposeIntent: (captured: number) => captured === generation,
    });
    render(<TasksPanel shell={shell} deployment={deployment} item="task-a" />);

    await act(async () => {
      screen.getByRole("button", { name: "Run task" }).click();
      await Promise.resolve();
    });
    expect(shell.runTask).toHaveBeenCalledWith({
      taskId: "task-a",
      agentDid: deployment.agentDid,
      args: {},
    });
    generation += 1;
    await act(async () => {
      resolve({ requestId: "stale-request", sessionId: "stale-session" });
      await pending;
    });

    expect(screen.queryByText("stale-request")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Run task" })).toBeEnabled();
  });

  it("preserves interval cadence and rejects non-positive intervals", async () => {
    const { api, shell } = harness();
    render(<SchedulesPanel shell={shell} deployment={deployment} item="timer-a" />);
    expectFields(["Display name", "Cadence", "Interval seconds", "Tags"]);
    const user = await replace("Interval seconds", "0");
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Interval seconds must be 1 or more",
    );
    expect(api.saveScheduleConfig).not.toHaveBeenCalled();

    await replace("Interval seconds", "90");
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(api.saveScheduleConfig).toHaveBeenCalledWith(
      expect.objectContaining({
        document: expect.objectContaining({
          cadence: { kind: "interval", interval_secs: 90 },
        }),
      }),
    );
  });

  it("runs a configured schedule through the typed bridge command", async () => {
    const { api, shell } = harness();
    render(<SchedulesPanel shell={shell} deployment={deployment} item="timer-a" />);

    await userEvent
      .setup()
      .click(screen.getByRole("button", { name: "Run schedule now" }));

    expect(api.runSchedule).toHaveBeenCalledWith({
      scheduleId: "timer-a",
      agentDid: deployment.agentDid,
    });
    expect(await screen.findByText("request-schedule")).toBeInTheDocument();
    expect(shell.refreshSnapshot).toHaveBeenCalledTimes(1);
  });

  it("validates grouped event invariants before persistence", async () => {
    const { api, shell } = harness();
    render(<EventSourcesPanel shell={shell} deployment={deployment} item="source-a" />);
    expectFields([
      "Display name",
      "Source collection",
      "Event kind",
      "Filter",
      "Correlation field",
      "Workspace authority",
      "Expected count",
      "Expected count source field",
      "Timeout seconds",
      "Minimum count",
      "Tags",
    ]);
    const user = await replace("Expected count", "2");
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Grouped events require a correlation field",
    );
    expect(api.saveEventSourceConfig).not.toHaveBeenCalled();
  });

  it("shows every trigger field and requires existing task/source references", () => {
    const { shell } = harness();
    render(<TriggersPanel shell={shell} deployment={deployment} item="trigger-a" />);
    expectFields([
      "Display name",
      "Description",
      "Task",
      "Source",
      "Schedule",
      "Concurrency",
      "Enabled",
      "Tags",
    ]);
  });

  describe("routes valid edits through each panel's canonical save command", () => {
    const cases: Array<{
      renderPanel: (shell: Shell) => React.ReactElement;
      field: string;
      value: string;
      method: string;
    }> = [
      {
        renderPanel: (shell) => (
          <BehaviorsPanel shell={shell} deployment={deployment} behaviorId="ops" />
        ),
        field: "Display name",
        value: "Ops edited",
        method: "saveBehaviorConfig",
      },
      {
        renderPanel: (shell) => (
          <ContextsPanel shell={shell} deployment={deployment} item="context-b" />
        ),
        field: "Description",
        value: "Edited context",
        method: "patchConfigComponents",
      },
      {
        renderPanel: (shell) => (
          <InferencePanel shell={shell} deployment={deployment} item="backend-a" />
        ),
        field: "Name",
        value: "Backend edited",
        method: "patchConfigComponents",
      },
      {
        renderPanel: (shell) => (
          <ProfilesPanel shell={shell} deployment={deployment} item="profile-a" />
        ),
        field: "Display name",
        value: "Profile edited",
        method: "applyConfigComponents",
      },
      {
        renderPanel: (shell) => (
          <ToolsPanel shell={shell} deployment={deployment} item="tools-a" />
        ),
        field: "Display name",
        value: "Tools edited",
        method: "saveToolsConfig",
      },
      {
        renderPanel: (shell) => (
          <ToolServicesPanel shell={shell} deployment={deployment} item="service-a" />
        ),
        field: "Display name",
        value: "Service edited",
        method: "saveToolServiceConfig",
      },
      {
        renderPanel: (shell) => (
          <SkillsPanel shell={shell} deployment={deployment} item="skill-a" />
        ),
        field: "Display name",
        value: "Skill edited",
        method: "saveSkillConfig",
      },
      {
        renderPanel: (shell) => (
          <TasksPanel shell={shell} deployment={deployment} item="task-a" />
        ),
        field: "Description",
        value: "Task edited",
        method: "saveTaskConfig",
      },
      {
        renderPanel: (shell) => (
          <EventSourcesPanel shell={shell} deployment={deployment} item="source-a" />
        ),
        field: "Display name",
        value: "Source edited",
        method: "saveEventSourceConfig",
      },
      {
        renderPanel: (shell) => (
          <TriggersPanel shell={shell} deployment={deployment} item="trigger-a" />
        ),
        field: "Display name",
        value: "Trigger edited",
        method: "saveTriggerConfig",
      },
    ];

    for (const testCase of cases) {
      it(testCase.value, async () => {
        const { api, shell } = harness();
        const view = render(testCase.renderPanel(shell));
        if (testCase.method === "applyConfigComponents") {
          await screen.findByLabelText("Temperature");
        }
        const user = await replace(testCase.field, testCase.value);
        await user.click(screen.getByRole("button", { name: "Save" }));
        expect(api[testCase.method], testCase.method).toHaveBeenCalledTimes(1);
        if (testCase.method === "saveSkillConfig") {
          expect(api.saveSkillConfig).toHaveBeenCalledWith(
            expect.objectContaining({
              document: expect.objectContaining({
                source_directory: "/skills/skill-a",
              }),
            }),
          );
        }
        view.unmount();
      });
    }
  });

  it("names the documents a delete leaves without their reference", async () => {
    const user = userEvent.setup();
    const cases: Array<[React.ReactElement, RegExp]> = [
      [
        <ProfilesPanel
          shell={harness().shell}
          deployment={deployment}
          item="profile-a"
        />,
        /Used by \d+ behaviors?; they lose this reference\./,
      ],
      [
        <SchedulesPanel
          shell={harness().shell}
          deployment={deployment}
          item="timer-a"
        />,
        /Used by 1 trigger; they lose this reference\./,
      ],
    ];
    for (const [panel, warning] of cases) {
      const view = render(panel);
      await user.click(
        screen.getByTestId("danger-zone").getElementsByTagName("button")[0]!,
      );
      expect(within(screen.getByRole("alertdialog")).getByText(warning)).toBeVisible();
      view.unmount();
    }
  });

  it("asks for the name before a list row's delete runs", async () => {
    const { api, shell } = harness();
    const user = userEvent.setup();
    render(<TasksPanel shell={shell} deployment={deployment} />);
    const name = deployment.tasks[0]!.name ?? deployment.tasks[0]!.taskId;
    await user.click(screen.getAllByRole("button", { name: `More for ${name}` })[0]!);
    await user.click(await screen.findByRole("menuitem", { name: /Delete/ }));
    const dialog = screen.getByRole("alertdialog");
    /* the row's warning is the same owner's wording as the Danger zone's */
    expect(
      within(dialog).getByText(/Used by 1 trigger; they lose this reference\./),
    ).toBeVisible();
    const button = within(dialog).getByRole("button", { name: /^Delete / });
    expect(button).toBeDisabled();
    await user.type(within(dialog).getByRole("textbox"), name);
    await user.click(button);
    expect(api.deleteTaskConfig).toHaveBeenCalledWith({
      taskId: deployment.tasks[0]!.taskId,
      agentDid: deployment.agentDid,
    });
  });

  it("routes every destructive panel action through its typed delete command", async () => {
    const cases: Array<{
      renderPanel: (shell: Shell) => React.ReactElement;
      method: string;
      request: Record<string, string>;
    }> = [
      {
        renderPanel: (shell) => (
          <BehaviorsPanel shell={shell} deployment={deployment} behaviorId="ops" />
        ),
        method: "deleteBehaviorConfig",
        request: { behaviorId: "ops", agentDid: deployment.agentDid },
      },
      {
        renderPanel: (shell) => (
          <InferencePanel shell={shell} deployment={deployment} item="backend-a" />
        ),
        method: "deleteBackendConfig",
        request: { backendId: "backend-a", agentDid: deployment.agentDid },
      },
      {
        renderPanel: (shell) => (
          <ProfilesPanel shell={shell} deployment={deployment} item="profile-a" />
        ),
        method: "deleteInferenceProfileConfig",
        request: { profileId: "profile-a", agentDid: deployment.agentDid },
      },
      {
        renderPanel: (shell) => (
          <ToolsPanel shell={shell} deployment={deployment} item="tools-b" />
        ),
        method: "deleteToolsConfig",
        request: { toolsId: "tools-b", agentDid: deployment.agentDid },
      },
      {
        renderPanel: (shell) => (
          <ToolServicesPanel shell={shell} deployment={deployment} item="service-a" />
        ),
        method: "deleteToolServiceConfig",
        request: { serviceId: "service-a", agentDid: deployment.agentDid },
      },
      {
        renderPanel: (shell) => (
          <SkillsPanel shell={shell} deployment={deployment} item="skill-a" />
        ),
        method: "deleteSkillConfig",
        request: { skillId: "skill-a", agentDid: deployment.agentDid },
      },
      {
        renderPanel: (shell) => (
          <TasksPanel shell={shell} deployment={deployment} item="task-b" />
        ),
        method: "deleteTaskConfig",
        request: { taskId: "task-b", agentDid: deployment.agentDid },
      },
      {
        renderPanel: (shell) => (
          <SchedulesPanel shell={shell} deployment={deployment} item="timer-a" />
        ),
        method: "deleteScheduleConfig",
        request: { scheduleId: "timer-a", agentDid: deployment.agentDid },
      },
      {
        renderPanel: (shell) => (
          <EventSourcesPanel shell={shell} deployment={deployment} item="source-a" />
        ),
        method: "deleteEventSourceConfig",
        request: { eventSourceId: "source-a", agentDid: deployment.agentDid },
      },
      {
        renderPanel: (shell) => (
          <TriggersPanel shell={shell} deployment={deployment} item="trigger-a" />
        ),
        method: "deleteTriggerConfig",
        request: { triggerId: "trigger-a", agentDid: deployment.agentDid },
      },
    ];

    for (const testCase of cases) {
      const { api, shell } = harness();
      const view = render(testCase.renderPanel(shell));
      const user = userEvent.setup();
      await user.click(
        screen.getByTestId("danger-zone").getElementsByTagName("button")[0]!,
      );
      expect(api[testCase.method], testCase.method).not.toHaveBeenCalled();
      /* every delete asks for the document's name, as it did before the sync */
      const confirmation = screen.getByRole("textbox", {
        name: /^Type .+ to confirm$/,
      });
      const accessibleName = confirmation.getAttribute("aria-label")!;
      expect(
        within(screen.getByRole("alertdialog")).getByRole("button", {
          name: /^Delete /,
        }),
        testCase.method,
      ).toBeDisabled();
      await user.type(
        confirmation,
        accessibleName.replace(/^Type /, "").replace(/ to confirm$/, ""),
      );
      await user.click(
        within(screen.getByRole("alertdialog")).getByRole("button", {
          name: /^Delete /,
        }),
      );
      expect(api[testCase.method], testCase.method).toHaveBeenCalledWith(
        testCase.request,
      );
      view.unmount();
    }
  });
});
