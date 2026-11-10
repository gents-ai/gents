import { screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { ToolsEditor } from "../src/ui/screens/agent/ToolsPanel";
import { toolsSentence } from "../src/ui/screens/agent/ToolsSummary";
import { renderIn, testApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";

const tools = deployment.tools[0]!;

describe("a tools page", () => {
  it("says what the tools allow in a sentence", () => {
    expect(
      toolsSentence(
        { ...tools, host: { bash: { mode: "Unrestricted", network_mode: "enabled" } } },
        { files: "ReadWrite", bash: "Unrestricted", root: "/work/app" },
      ),
    ).toBe(
      "Can read and edit files in /work/app, run any command and reach the network.",
    );
    expect(
      toolsSentence(tools, { files: "ReadOnly", bash: "ReadOnly", root: "" }),
    ).toBe("Can read files in its workspace, run read-only commands, with no network.");
    expect(toolsSentence(tools, { files: "Off", bash: "Off", root: "" })).toBe(
      "Can't touch files or run commands.",
    );
    expect(toolsSentence(tools, { files: "ReadOnly", bash: "Off", root: "" })).toBe(
      "Can read files in its workspace, but run no commands.",
    );
  });

  it("opens on the sentence and who uses the tools, the form below", () => {
    renderIn(
      testApp({ deployments: [deployment] }),
      <ToolsEditor deployment={deployment} tools={tools} />,
    );
    expect(screen.getByRole("heading", { name: "Tools A" })).toBeVisible();
    expect(screen.getByText(/^Can read files in its workspace/)).toBeVisible();
    expect(screen.getByText(/^1 agent: Default$/)).toBeVisible();
    expect(screen.getByText("Files and commands")).toBeVisible();
  });
});
