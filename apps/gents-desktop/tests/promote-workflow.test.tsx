import { fireEvent, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { DynamicWorkflow } from "@source-inc/gents-desktop-client";

import { PromoteWorkflowDialog } from "../src/ui/screens/PromoteWorkflowDialog";
import { renderIn, testApp } from "./app-fixture";

const SELECTOR = "session-1:call-1";

const workflow = (handle: string, script = "export default 1;"): DynamicWorkflow =>
  ({
    view: {
      address: SELECTOR,
      handle,
      state: "completed",
      outcome: { code: "ok", message: "Rome wins the debate" },
      pin: { agent_budget: 6, origin: "dynamic" },
      children: [
        {
          slot: "pro",
          title: "argue for",
          state: "completed",
          target: "local:debater",
        },
      ],
    },
    script,
    targets: ["local:debater"],
    owners: [],
    promoted_as: [],
  }) as unknown as DynamicWorkflow;

function mount(api: Record<string, unknown>, onClose = vi.fn()) {
  renderIn(
    testApp({ api }),
    <PromoteWorkflowDialog selector={SELECTOR} onClose={onClose} />,
  );
  return onClose;
}

const nameField = () => screen.findByRole("textbox", { name: "Tool name" });
const promote = () => screen.getByRole("button", { name: "Promote" });

describe("PromoteWorkflowDialog", () => {
  it("shows the script, the children with their targets, the result and the ceiling", async () => {
    mount({ getDynamicWorkflow: vi.fn().mockResolvedValue(workflow("rome_debate")) });
    expect((await screen.findByTestId("promote-script")).textContent).toContain(
      "export default 1;",
    );
    expect(screen.getByTestId("promote-children").textContent).toContain(
      "local:debater",
    );
    expect(screen.getByTestId("promote-result").textContent).toContain(
      "Rome wins the debate",
    );
    expect(screen.getByTestId("promote-ceiling").textContent).toContain("6");
  });

  it("collapses a script past 40 lines", async () => {
    const long = Array.from({ length: 50 }, (_, i) => `line ${i + 1}`).join("\n");
    mount({ getDynamicWorkflow: vi.fn().mockResolvedValue(workflow("x", long)) });
    const script = await screen.findByTestId("promote-script");
    expect(script.textContent).not.toContain("line 50");
    fireEvent.click(screen.getByRole("button", { name: "Show all 50 lines" }));
    expect(screen.getByTestId("promote-script").textContent).toContain("line 50");
  });

  it("leaves a handle that is not a tool name empty, with Promote disabled", async () => {
    mount({ getDynamicWorkflow: vi.fn().mockResolvedValue(workflow("rome-debate")) });
    expect(((await nameField()) as HTMLInputElement).value).toBe("");
    expect(promote()).toBeDisabled();
  });

  it("prefills a valid handle and installs it on Promote", async () => {
    const promoteWorkflow = vi.fn().mockResolvedValue({ tool: "gents/rome_debate" });
    mount({
      getDynamicWorkflow: vi.fn().mockResolvedValue(workflow("rome_debate")),
      promoteWorkflow,
    });
    expect(((await nameField()) as HTMLInputElement).value).toBe("rome_debate");
    fireEvent.click(promote());
    expect(await screen.findByText("Installed as gents/rome_debate")).toBeTruthy();
    expect(promoteWorkflow).toHaveBeenCalledWith({
      selector: SELECTOR,
      name: "rome_debate",
      agent: null,
    });
  });

  it("offers only Done on a run whose script is already promoted", async () => {
    const run = { ...workflow("rome_debate"), promoted_as: ["gents/rome_debate"] };
    mount({ getDynamicWorkflow: vi.fn().mockResolvedValue(run) });
    expect((await screen.findByTestId("promote-already")).textContent).toContain(
      "gents/rome_debate",
    );
    expect(screen.queryByRole("textbox", { name: "Tool name" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Promote" })).toBeNull();
    expect(screen.getByRole("button", { name: "Done" })).toBeTruthy();
  });

  it("shows a refusal's message in place of the button and no install link", async () => {
    // the bridge folds a facade error into one message, its code first
    const message = "promote_javy_missing: javy is not on PATH";
    mount({
      getDynamicWorkflow: vi.fn().mockResolvedValue(workflow("rome_debate")),
      promoteWorkflow: vi.fn().mockRejectedValue(new Error(message)),
    });
    await nameField();
    fireEvent.click(promote());
    expect(await screen.findByText(message)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Promote" })).toBeNull();
    expect(screen.queryByText(/Installed as/)).toBeNull();
  });
});
