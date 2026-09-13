import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";
import type { Shell } from "../src/ui/hooks/useShell";
import { AgentPanel } from "../src/ui/screens/agent/AgentPanel";
import { BehaviorsPanel } from "../src/ui/screens/agent/BehaviorsPanel";
import { ContextsPanel } from "../src/ui/screens/agent/ContextsPanel";
import { EventSourcesPanel } from "../src/ui/screens/agent/EventSourcesPanel";
import { InferencePanel } from "../src/ui/screens/agent/InferencePanel";
import { ProfilesPanel } from "../src/ui/screens/agent/ProfilesPanel";
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
    saveBehaviorConfig: vi.fn().mockResolvedValue({}),
    patchConfigComponents: vi.fn().mockResolvedValue({}),
    applyConfigComponents: vi.fn().mockResolvedValue({}),
    saveBackendConfig: vi.fn().mockResolvedValue({}),
    deleteBackendConfig: vi.fn().mockResolvedValue({}),
    probeInferenceEndpoint: vi
      .fn()
      .mockResolvedValue({ reachable: true, models: ["model-a"] }),
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
  };
  const shell = {
    api: api as unknown as DesktopApiAdapter,
    snapshot: { bootstrap },
    saveAgentConfig: api.saveAgentConfig,
    saveBehaviorConfig: api.saveBehaviorConfig,
    applyConfig: (run: (bridge: DesktopApiAdapter) => Promise<unknown>) =>
      run(api as unknown as DesktopApiAdapter),
    refreshSnapshot: vi.fn().mockResolvedValue(undefined),
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
  it("requires the agent identity fields and saves editable principal tags", async () => {
    const { api, shell } = harness();
    render(<AgentPanel shell={shell} deployment={deployment} />);
    expectFields(["Display name", "Default behaviour", "Enabled", "Tags"]);

    const user = await replace("Display name", " ");
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Display name is required",
    );
    expect(api.saveAgentConfig).not.toHaveBeenCalled();

    await replace("Display name", "Acceptance Agent");
    await replace("Tags", "acceptance\ndesktop");
    await user.click(screen.getByRole("button", { name: "Save changes" }));
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
      "Context",
      "Inference profile",
      "Enabled",
      "Default behaviour",
      "Tags",
    ]);
    const user = await replace("Display name", " ");
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Display name is required",
    );
    expect(api.saveBehaviorConfig).not.toHaveBeenCalled();
  });

  it("creates behavior scaffolds disabled until the operator saves them", async () => {
    const { api, shell } = harness();
    render(<BehaviorsPanel shell={shell} deployment={deployment} />);

    await userEvent
      .setup()
      .click(screen.getByRole("button", { name: "New behaviour" }));

    expect(api.saveBehaviorConfig).toHaveBeenCalledWith(
      expect.objectContaining({
        document: expect.objectContaining({ enabled: false }),
      }),
    );
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
      "Skill IDs",
      "Tags",
    ]);
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Delete" }));
    expect(api.deleteContextConfig).not.toHaveBeenCalled();
    expect(screen.getByText(/Delete .+\?/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Delete now" }));
    expect(api.deleteContextConfig).toHaveBeenCalledWith({
      contextId: "context-b",
      agentDid: deployment.agentDid,
    });
    expect(api.applyConfigComponents).not.toHaveBeenCalled();
  });

  it("creates only the new context and represents empty lists as null", async () => {
    const { api, shell } = harness();
    render(<ContextsPanel shell={shell} deployment={deployment} />);

    await userEvent.setup().click(screen.getByRole("button", { name: "New context" }));

    expect(api.applyConfigComponents).toHaveBeenCalledTimes(1);
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
    await user.click(screen.getByRole("button", { name: "Save changes" }));
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
        provider: "xai",
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

  it("validates the full inference profile, sampling, and execution surface", async () => {
    const { api, shell } = harness();
    render(<ProfilesPanel shell={shell} deployment={deployment} item="profile-a" />);
    expectFields([
      "Display name",
      "Description",
      "Backend",
      "Model",
      "Reasoning effort",
      "Context window",
      "Max output tokens",
      "Sampling document ID",
      "Temperature",
      "Top P",
      "Top K",
      "Seed",
      "Min P",
      "Frequency penalty",
      "Presence penalty",
      "Repetition penalty",
      "Execution document ID",
      "Max turns",
      "Max total tokens",
      "Stream batch ms",
      "Stream liveness seconds",
      "Deadline seconds",
      "Retry policy ID",
      "Tags",
    ]);
    const user = await replace("Top P", "1.5");
    await replace("Sampling document ID", "sampling-a");
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Top P must be 1 or less",
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
    await user.click(screen.getByRole("button", { name: "Save changes" }));
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
    await user.click(screen.getByRole("button", { name: "Save changes" }));
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
    await user.click(screen.getByRole("button", { name: "Save changes" }));
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
      "Behaviour",
      "Enabled",
      "Description",
      "Prompt template",
      "Durable goal objective",
      "Goal token budget",
      "Output schema ref",
      "Task hooks",
      "Tags",
      "Args",
    ]);
    const user = await replace("Prompt template", " ");
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Prompt template is required",
    );
    expect(api.saveTaskConfig).not.toHaveBeenCalled();
  });

  it("preserves interval cadence and rejects non-positive intervals", async () => {
    const { api, shell } = harness();
    render(<SchedulesPanel shell={shell} deployment={deployment} item="timer-a" />);
    expectFields(["Display name", "Cadence", "Interval seconds", "Tags"]);
    const user = await replace("Interval seconds", "0");
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Interval seconds must be 1 or more",
    );
    expect(api.saveScheduleConfig).not.toHaveBeenCalled();

    await replace("Interval seconds", "90");
    await user.click(screen.getByRole("button", { name: "Save changes" }));
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

    expect(api.runSchedule).toHaveBeenCalledWith({ scheduleId: "timer-a" });
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
    await user.click(screen.getByRole("button", { name: "Save changes" }));
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

  it("routes valid edits through every panel's canonical save command", async () => {
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
      const { api, shell } = harness();
      const view = render(testCase.renderPanel(shell));
      const user = await replace(testCase.field, testCase.value);
      await user.click(screen.getByRole("button", { name: "Save changes" }));
      expect(api[testCase.method], testCase.method).toHaveBeenCalledTimes(1);
      view.unmount();
    }
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
      await user.click(screen.getByRole("button", { name: "Delete" }));
      expect(api[testCase.method], testCase.method).not.toHaveBeenCalled();
      await user.click(screen.getByRole("button", { name: "Delete now" }));
      expect(api[testCase.method], testCase.method).toHaveBeenCalledWith(
        testCase.request,
      );
      view.unmount();
    }
  });
});
