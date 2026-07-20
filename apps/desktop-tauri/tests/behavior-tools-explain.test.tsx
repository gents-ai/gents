import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { BehaviorToolSurface } from "../src/components/config/BehaviorToolSurface";
import { setDesktopApiAdapterForTests } from "../src/lib/desktop-api";
import type { DesktopApiAdapter } from "../src/lib/desktop-api";

function withExplanation(payload: unknown, fail = false) {
  setDesktopApiAdapterForTests({
    explainToolSurface: fail
      ? vi.fn().mockRejectedValue(new Error("remote agents not yet supported"))
      : vi.fn().mockResolvedValue(payload),
  } as unknown as DesktopApiAdapter);
}

describe("behavior tool surface", () => {
  afterEach(() => setDesktopApiAdapterForTests(null));

  it("stays collapsed until opened, then renders the resolved surface", async () => {
    withExplanation({
      behaviorId: "default",
      enabled: true,
      toolSelectionSource: "document",
      toolPolicySemantics: "tool-policy/v1",
      ceilingSource: "init_json",
      mcpServicesOnline: false,
      surface: {
        tool_names: ["read_file", "defra_exec"],
        included: {},
        excluded: { write_file: ["ceiling is readonly"] },
        unavailable: { mcp_call: ["no MCP services online"] },
        warnings: [{ code: "w1", message: "subagent target inactive" }],
      },
    });
    render(<BehaviorToolSurface agentDid="did:a" behaviorId="default" />);

    expect(screen.queryByTestId("behavior-tools-explain-refresh")).toBeNull();
    fireEvent.click(screen.getByTestId("behavior-tools-explain-toggle"));

    await waitFor(() =>
      expect(screen.getByTestId("resolved-tool-names")).toHaveTextContent("read_file"),
    );
    expect(screen.getByText(/write_file/)).toBeInTheDocument();
    expect(screen.getByText(/ceiling is readonly/)).toBeInTheDocument();
    expect(screen.getByText(/no MCP services online/)).toBeInTheDocument();
    expect(screen.getByText("subagent target inactive")).toBeInTheDocument();
    expect(screen.getByText(/MCP services offline/)).toBeInTheDocument();
  });

  it("surfaces explanation failures with retry", async () => {
    withExplanation(null, true);
    render(<BehaviorToolSurface agentDid="did:a" behaviorId="default" />);
    fireEvent.click(screen.getByTestId("behavior-tools-explain-toggle"));

    await waitFor(() =>
      expect(screen.getByTestId("behavior-tools-explain-error")).toHaveTextContent(
        "remote agents not yet supported",
      ),
    );
    expect(screen.getByTestId("behavior-tools-explain-refresh")).toBeEnabled();
  });

  it("renders nothing for an unsaved behavior", () => {
    withExplanation({});
    const { container } = render(
      <BehaviorToolSurface agentDid="did:a" behaviorId={null} />,
    );
    expect(container.firstChild).toBeNull();
  });
});
