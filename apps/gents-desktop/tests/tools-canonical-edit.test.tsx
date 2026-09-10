import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { Tools } from "@source-inc/gents-desktop-client";
import { ToolsConfigEditor } from "../src/components/config/ToolsConfigPanel";
const tools: Tools = {
  agent_did: "owner",
  tools_id: " tools ",
  host: {
    root: "/work",
    files: { mode: "ReadOnly", timeout_secs: 33 },
    bash: { mode: "Off", timeout_secs: 60 },
  },
  remote: {
    services: [
      {
        mcp_service_id: "service",
        tool_names: ["read"],
        style: "discovery",
        timeout_secs: 9,
      },
    ],
  },
  tags: ["preserve"],
};
function props() {
  return {
    agentDid: "owner",
    tools,
    toolServices: [{ agent_did: "owner", service_id: "service" }],
    subagentTargets: [],
    saving: false,
    savedStatus: null,
    onApplyConfigComponents: vi.fn().mockResolvedValue(undefined),
    onDeleteToolsConfig: vi.fn(),
    onDeleted: vi.fn(),
    onSaved: vi.fn(),
  };
}
describe("canonical tools authoring", () => {
  it("changes presentation without expanding permissions or dropping per-service timeout", async () => {
    const handlers = props();
    render(<ToolsConfigEditor {...handlers} />);
    fireEvent.change(screen.getByTestId("tools-service-style-service"), {
      target: { value: "flat" },
    });
    fireEvent.click(screen.getByTestId("tools-save"));
    await waitFor(() =>
      expect(handlers.onApplyConfigComponents).toHaveBeenCalledTimes(1),
    );
    const { document } = handlers.onApplyConfigComponents.mock.calls[0][0];
    expect(document.tools[0]).toEqual({
      ...tools,
      remote: { services: [{ ...tools.remote!.services![0], style: "flat" }] },
    });
    expect(document.tools[0].tools_id).toBe(" tools ");
  });
  it("configuring a service starts with no tool grants", async () => {
    const handlers = props();
    render(
      <ToolsConfigEditor
        {...handlers}
        tools={{ agent_did: "owner", tools_id: "tools" }}
      />,
    );
    fireEvent.click(screen.getByTestId("tools-service-service"));
    fireEvent.click(screen.getByTestId("tools-save"));
    await waitFor(() =>
      expect(handlers.onApplyConfigComponents).toHaveBeenCalledTimes(1),
    );
    expect(
      handlers.onApplyConfigComponents.mock.calls[0][0].document.tools[0].remote
        .services,
    ).toEqual([{ mcp_service_id: "service", tool_names: [] }]);
  });
  it("applies canonical target documents without implicitly selecting them", async () => {
    const handlers = props();
    render(<ToolsConfigEditor {...handlers} />);
    const target = {
      agent_did: "owner",
      target_id: "target",
      target_agent_did: "other",
      behavior_id: "worker",
      name: "review",
    };
    fireEvent.change(screen.getByTestId("tools-target-documents"), {
      target: { value: JSON.stringify([target]) },
    });
    fireEvent.click(screen.getByTestId("tools-save"));
    await waitFor(() =>
      expect(handlers.onApplyConfigComponents).toHaveBeenCalledTimes(1),
    );
    expect(
      handlers.onApplyConfigComponents.mock.calls[0][0].document.subagent_targets,
    ).toEqual([target]);
    expect(
      handlers.onApplyConfigComponents.mock.calls[0][0].document.tools[0].subagents,
    ).toBeUndefined();
  });
});
