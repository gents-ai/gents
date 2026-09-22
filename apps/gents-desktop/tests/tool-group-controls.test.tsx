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
      await user.click(screen.getByRole("button", { name: "Save" }));
      expect(api.saveToolsConfig).not.toHaveBeenCalled();
    }
    fireEvent.change(timeout, { target: { value: "15" } });
    await user.click(screen.getByRole("button", { name: "Save" }));
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
    await user.click(screen.getByRole("button", { name: "Save" }));
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
    await user.click(screen.getByRole("button", { name: "Save" }));
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
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(api.saveToolsConfig.mock.calls[0][0].document.remote.services).toEqual([
      { mcp_service_id: "service-a", tool_names: null },
    ]);
    await user.click(screen.getByRole("textbox", { name: "Service A tool names" }));
    await user.paste("read\nse?rch");
    await user.click(screen.getByRole("button", { name: "Save" }));
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
    await user.click(screen.getByRole("button", { name: "Save" }));
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
    await user.click(screen.getByRole("button", { name: "Save" }));
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
    await user.click(screen.getByRole("button", { name: "Save" }));
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
    await user.click(screen.getByRole("button", { name: "Save" }));
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
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Subagent wait timeout maximum",
    );
    expect(api.saveToolsConfig).not.toHaveBeenCalled();
  });

  it.each([
    ["bash", { host: { bash: { max_timeout_secs: 5 } } }, "Bash timeout"],
    [
      "bash wait",
      { host: { bash: { max_wait_timeout_secs: 5 } } },
      "Bash wait timeout",
    ],
    ["subagent wait", { subagents: { max_wait_timeout_secs: 5 } }, "Subagent wait"],
    [
      "language server",
      { integrations: { lsp: { max_timeout_secs: 5 } } },
      "Language server timeout",
    ],
    [
      "remote wait",
      {
        remote: {
          services: [{ mcp_service_id: "service-a", max_wait_timeout_secs: 5 }],
        },
      },
      "Remote wait timeout",
    ],
  ])(
    "rejects a %s ceiling below its canonical effective default",
    async (_name, groups, message) => {
      const { api, shell } = harness();
      const user = userEvent.setup();
      render(<ToolsPanel shell={shell} deployment={deployment} item="tools-a" />);
      fireEvent.change(screen.getByRole("textbox", { name: "Canonical JSON" }), {
        target: { value: JSON.stringify(groups) },
      });
      await user.click(screen.getByRole("button", { name: "Save" }));
      expect(await screen.findByRole("alert")).toHaveTextContent(message);
      expect(api.saveToolsConfig).not.toHaveBeenCalled();
    },
  );

  it("preserves valid independent remote call and stale-health caps", async () => {
    const { api, shell } = harness();
    const user = userEvent.setup();
    render(<ToolsPanel shell={shell} deployment={deployment} item="tools-a" />);
    fireEvent.change(screen.getByRole("textbox", { name: "Canonical JSON" }), {
      target: {
        value: JSON.stringify({
          remote: {
            services: [
              {
                mcp_service_id: "service-a",
                timeout_secs: 1,
                stale_timeout_secs: 120,
              },
            ],
          },
        }),
      },
    });
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(api.saveToolsConfig).toHaveBeenCalledTimes(1);
    expect(api.saveToolsConfig.mock.calls[0][0].document.remote.services[0]).toEqual(
      expect.objectContaining({ timeout_secs: 1, stale_timeout_secs: 120 }),
    );
  });

  it("keeps malformed JSON editable and blocks persistence", async () => {
    const { api, shell } = harness();
    const user = userEvent.setup();
    render(<ToolsPanel shell={shell} deployment={deployment} item="tools-a" />);
    await user.clear(screen.getByRole("textbox", { name: "Canonical JSON" }));
    expect(screen.getByRole("alert")).toHaveTextContent("Repair");
    await user.click(screen.getByRole("button", { name: "Save" }));
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
    await user.type(screen.getByRole("combobox", { name: "Skills" }), "review");
    await user.click(screen.getByRole("option", { name: "Code review" }));
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(
      api.patchConfigComponents.mock.calls[0][0].patches[0].changes.skill_ids,
    ).toContain("skill-doc");
  });
});
