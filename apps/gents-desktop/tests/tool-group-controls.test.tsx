import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeAll, afterAll, describe, expect, it, vi } from "vitest";
import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";
import type { Shell } from "../src/ui/hooks/useShell";
import { ToolsPanel } from "../src/ui/screens/agent/ToolsPanel";
import { ContextsPanel } from "../src/ui/screens/agent/ContextsPanel";
import { deployment } from "./config-panel-wiring/fixtures";
const originalPointerEvent = window.PointerEvent;
beforeAll(() => {
  window.PointerEvent = MouseEvent as typeof PointerEvent;
});
afterAll(() => {
  window.PointerEvent = originalPointerEvent;
});

function harness() {
  const api = {
    saveToolsConfig: vi.fn().mockResolvedValue({}),
    applyConfigComponents: vi.fn().mockResolvedValue({}),
    patchConfigComponents: vi.fn().mockResolvedValue({}),
    testToolService: vi.fn().mockResolvedValue({ tools: [], error: null }),
  };
  const shell = {
    api,
    applyConfig: (run: (api: DesktopApiAdapter) => Promise<unknown>) =>
      run(api as unknown as DesktopApiAdapter),
  } as unknown as Shell;
  return { api, shell };
}

describe("canonical tool selections", () => {
  it("keeps invalid timeout text visible and blocks unrelated saves until corrected", async () => {
    const { api, shell } = harness();
    const user = userEvent.setup();
    render(<ToolsPanel shell={shell} deployment={deployment} item="tools-a" />);
    const timeout = screen.getByRole("textbox", {
      name: "File operation timeout seconds",
    });
    await user.type(screen.getByRole("textbox", { name: "Display name" }), " edited");
    for (const invalid of ["0", "-1", "1.5", "abc", "9007199254740992"]) {
      fireEvent.change(timeout, { target: { value: invalid } });
      expect(timeout).toHaveValue(invalid);
      expect(screen.getByRole("alert")).toHaveTextContent("positive whole number");
      await user.click(screen.getByRole("button", { name: "Save changes" }));
      expect(api.saveToolsConfig).not.toHaveBeenCalled();
    }
    fireEvent.change(timeout, { target: { value: "15" } });
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(api.saveToolsConfig.mock.calls[0][0].document.host.files.timeout_secs).toBe(
      15,
    );
  });

  it("cancels invalid guided limits without changing canonical settings", async () => {
    const { api, shell } = harness();
    const user = userEvent.setup();
    render(<ToolsPanel shell={shell} deployment={deployment} item="tools-a" />);
    const field = () =>
      screen.getByRole("textbox", { name: "File operation timeout seconds" });
    const original = (field() as HTMLInputElement).value;
    fireEvent.change(field(), { target: { value: "nope" } });
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(field()).toHaveValue(original);
    expect(screen.queryByRole("alert")).toBeNull();
    expect(api.saveToolsConfig).not.toHaveBeenCalled();
  });

  it("keeps independent opt-ins off and persists only an explicitly selected permission", async () => {
    const { api, shell } = harness();
    const user = userEvent.setup();
    render(<ToolsPanel shell={shell} deployment={deployment} item="tools-a" />);
    expect(
      screen.getByRole("switch", { name: "Install graph packs" }),
    ).not.toBeChecked();
    await user.click(screen.getByRole("switch", { name: "Graph tools" }));
    expect(api.saveToolsConfig).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    const tools = api.saveToolsConfig.mock.calls[0][0].document;
    expect(tools.built_ins).toEqual({ enable_graph_tools: true });
    expect(tools.self_config).toBeNull();
    expect(tools.host.files.mode).toBe("ReadOnly");
  });

  it("creates and selects a local subagent target atomically without granting spawn implicitly", async () => {
    const { api, shell } = harness();
    const user = userEvent.setup();
    render(
      <ToolsPanel
        shell={shell}
        deployment={{ ...deployment, subagentTargets: [] }}
        item="tools-a"
      />,
    );
    await user.click(
      screen.getByRole("combobox", { name: "Add a local delegation target" }),
    );
    await user.click(await screen.findByRole("option", { name: "Ops" }));
    await user.click(screen.getByRole("checkbox", { name: /Ops/ }));
    expect(api.applyConfigComponents).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    await waitFor(() => expect(api.applyConfigComponents).toHaveBeenCalledTimes(1));
    const pack = api.applyConfigComponents.mock.calls[0][0].document;
    expect(pack.subagent_targets[0]).toMatchObject({
      agent_did: deployment.agentDid,
      target_agent_did: deployment.agentDid,
      behavior_id: "ops",
      name: "Ops",
    });
    expect(pack.tools[0].subagents).toEqual({
      target_ids: [pack.subagent_targets[0].target_id],
    });
    expect(api.saveToolsConfig).not.toHaveBeenCalled();
  });

  it("selects remote services without granting discovered tools and rejects wildcards", async () => {
    const { api, shell } = harness();
    const user = userEvent.setup();
    render(<ToolsPanel shell={shell} deployment={deployment} item="tools-a" />);
    await user.click(screen.getByRole("checkbox", { name: "Service A" }));
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(api.saveToolsConfig.mock.calls[0][0].document.remote.services).toEqual([
      { mcp_service_id: "service-a", tool_names: null },
    ]);
    await user.type(
      screen.getByRole("textbox", { name: "Service A tool names" }),
      "read\n*",
    );
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("exact names");
    expect(api.saveToolsConfig).toHaveBeenCalledTimes(1);
  });

  it("shows discovered remote tools without granting them implicitly", async () => {
    const { api, shell } = harness();
    api.testToolService.mockResolvedValue({
      tools: [{ name: "search", description: "Search the service" }],
      error: null,
    });
    const user = userEvent.setup();
    render(<ToolsPanel shell={shell} deployment={deployment} item="tools-a" />);
    await user.click(screen.getByRole("checkbox", { name: "Service A" }));
    await user.click(screen.getByRole("button", { name: "Discover remote tools" }));
    expect(await screen.findByRole("checkbox", { name: /search/ })).not.toBeChecked();
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(api.saveToolsConfig.mock.calls[0][0].document.remote.services).toEqual([
      { mcp_service_id: "service-a", tool_names: null },
    ]);
  });

  it("reports remote discovery errors without changing the grant", async () => {
    const { api, shell } = harness();
    api.testToolService.mockResolvedValue({
      tools: [],
      error: "service handshake failed",
    });
    const user = userEvent.setup();
    render(<ToolsPanel shell={shell} deployment={deployment} item="tools-a" />);
    await user.click(screen.getByRole("checkbox", { name: "Service A" }));
    await user.click(screen.getByRole("button", { name: "Discover remote tools" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "service handshake failed",
    );
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(api.saveToolsConfig.mock.calls[0][0].document.remote.services[0]).toEqual({
      mcp_service_id: "service-a",
      tool_names: null,
    });
  });

  it("preserves unedited advanced host fields through guided save and reload", async () => {
    const { api, shell } = harness();
    const user = userEvent.setup();
    const configured = {
      ...deployment.tools[0],
      host: {
        ...deployment.tools[0].host,
        cli: [{ name: "git", timeout_secs: 12 }],
        bash: {
          ...deployment.tools[0].host?.bash,
          allowed_argv_prefixes: [["git", "status"]],
        },
      },
      tags: ["reviewed"],
    };
    const view = { ...deployment, tools: [configured] };
    const rendered = render(
      <ToolsPanel shell={shell} deployment={view} item="tools-a" />,
    );
    await user.click(screen.getByRole("switch", { name: "Memory" }));
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    const saved = api.saveToolsConfig.mock.calls[0][0].document;
    expect(saved.host.cli).toEqual([{ name: "git", timeout_secs: 12 }]);
    expect(saved.host.bash.allowed_argv_prefixes).toEqual([["git", "status"]]);
    expect(saved.tags).toEqual(["reviewed"]);
    rendered.rerender(
      <ToolsPanel
        shell={shell}
        deployment={{ ...view, tools: [saved] }}
        item="tools-a"
      />,
    );
    await waitFor(() =>
      expect(screen.getByRole("switch", { name: "Memory" })).toBeChecked(),
    );
  });

  it("clearing selected remote tools also clears background grants", async () => {
    const { api, shell } = harness();
    const user = userEvent.setup();
    const configured = {
      ...deployment.tools[0],
      remote: {
        services: [
          {
            mcp_service_id: "service-a",
            tool_names: ["search"],
            background_tool_names: ["search"],
          },
        ],
      },
    };
    render(
      <ToolsPanel
        shell={shell}
        deployment={{ ...deployment, tools: [configured] }}
        item="tools-a"
      />,
    );
    await user.clear(screen.getByRole("textbox", { name: "Service A tool names" }));
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(
      api.saveToolsConfig.mock.calls[0][0].document.remote.services[0],
    ).toMatchObject({ tool_names: null, background_tool_names: null });
  });

  it("rejects inconsistent timeout ceilings before persistence", async () => {
    const { api, shell } = harness();
    const user = userEvent.setup();
    render(<ToolsPanel shell={shell} deployment={deployment} item="tools-a" />);
    const editor = screen.getByRole("textbox", { name: "Canonical JSON" });
    const advanced = JSON.parse((editor as HTMLTextAreaElement).value);
    advanced.subagents = { wait_timeout_secs: 60, max_wait_timeout_secs: 30 };
    fireEvent.change(editor, { target: { value: JSON.stringify(advanced) } });
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Subagent wait timeout maximum",
    );
    expect(api.saveToolsConfig).not.toHaveBeenCalled();
  });

  it("keeps malformed JSON editable and blocks persistence", async () => {
    const { api, shell } = harness();
    const user = userEvent.setup();
    render(<ToolsPanel shell={shell} deployment={deployment} item="tools-a" />);
    await user.clear(screen.getByRole("textbox", { name: "Canonical JSON" }));
    expect(screen.getByRole("alert")).toHaveTextContent("Repair");
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(api.saveToolsConfig).not.toHaveBeenCalled();
  });

  it("uses searchable skill names while persisting canonical IDs", async () => {
    const { api, shell } = harness();
    const user = userEvent.setup();
    const skills = [
      {
        ...deployment.skills[0],
        skillId: "skill-doc",
        name: "Code review",
        displayName: "Code review",
        description: "Read-only review",
      },
    ];
    render(
      <ContextsPanel
        shell={shell}
        deployment={{ ...deployment, skills }}
        item="context-b"
      />,
    );
    await user.type(screen.getByRole("textbox", { name: "Search skills" }), "review");
    await user.click(screen.getByRole("checkbox", { name: /Code review/ }));
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    expect(
      api.patchConfigComponents.mock.calls[0][0].patches[0].changes.skill_ids,
    ).toContain("skill-doc");
  });
});
