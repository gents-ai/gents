import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { CodeContextHeader } from "../src/components/code/CodeContextHeader";
import { deployment } from "./config-panel-wiring/fixtures";

describe("CodeContextHeader", () => {
  it("surfaces the working directory and permission boundary of the governing selection", () => {
    const codingDeployment = {
      ...deployment,
      toolSelections: [
        {
          ...deployment.toolSelections[0],
          fileToolRoot: "/srv/project",
          fileToolsMode: "ReadWrite",
          bashMode: "ReadOnly",
          commandNetworkMode: "disabled",
        },
        deployment.toolSelections[1],
      ],
    };
    render(<CodeContextHeader deployment={codingDeployment} onBackToChat={vi.fn()} />);
    expect(screen.getByTestId("code-context-workdir")).toHaveTextContent(
      "/srv/project",
    );
    expect(screen.getByTestId("code-context-files")).toHaveTextContent("read / write");
    expect(screen.getByTestId("code-context-bash")).toHaveTextContent("read-only");
    expect(screen.getByTestId("code-context-host")).toHaveTextContent("Local Agent");
    expect(screen.getByTestId("code-context-host")).toHaveTextContent("peer-1");
  });

  it("resolves the SELECTED behavior's tool selection, not the default or [0]", () => {
    render(
      <CodeContextHeader
        deployment={deployment}
        selectedBehaviorId="ops"
        onBackToChat={vi.fn()}
      />,
    );
    expect(screen.getByTestId("code-context-files")).toHaveTextContent("off");
    expect(screen.getByTestId("code-context-bash")).toHaveTextContent("off");
  });

  it("reports files & bash off when the governing behavior resolves no selection", () => {
    const noSelection = {
      ...deployment,
      behaviors: [{ ...deployment.behaviors[0], toolSelectionId: null }],
    };
    render(<CodeContextHeader deployment={noSelection} onBackToChat={vi.fn()} />);
    expect(screen.getByTestId("code-context-workdir")).toHaveTextContent(
      "none (files & bash off)",
    );
    expect(screen.getByTestId("code-context-files")).toHaveTextContent("off");
  });

  it("falls back gracefully when no agent is selected", () => {
    render(<CodeContextHeader deployment={null} onBackToChat={vi.fn()} />);
    expect(screen.getByTestId("code-context-host")).toHaveTextContent("—");
    expect(screen.getByTestId("code-context-workdir")).toHaveTextContent(
      "none (files & bash off)",
    );
  });

  it("returns to chat when Back to Chat is clicked", () => {
    const onBackToChat = vi.fn();
    render(<CodeContextHeader deployment={deployment} onBackToChat={onBackToChat} />);
    fireEvent.click(screen.getByTestId("code-back-to-chat"));
    expect(onBackToChat).toHaveBeenCalledTimes(1);
  });
});
