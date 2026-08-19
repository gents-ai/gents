import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { ConfigWorkspace } from "../src/components/ConfigWorkspace";
import { deployment } from "./config-panel-wiring/fixtures";

function renderWorkspace(backLabel?: string, onBack = vi.fn()) {
  render(
    <ConfigWorkspace
      backLabel={backLabel}
      bootstrap={null}
      selectedDeployment={deployment}
      selectedBehaviorId={null}
      saving={false}
      runningTask={false}
      onBack={onBack}
      onDirtyChange={() => undefined}
      onDeleteSkillConfig={vi.fn()}
      onSaveAgentConfig={vi.fn()}
      onRunTask={vi.fn()}
      onSaveBackendConfig={vi.fn()}
      onSaveBehaviorConfig={vi.fn()}
      onSaveEventTriggerConfig={vi.fn()}
      onSaveInferenceProfileConfig={vi.fn()}
      onSaveScheduleConfig={vi.fn()}
      onSaveSkillConfig={vi.fn()}
      onSaveTaskConfig={vi.fn()}
      onSaveToolSelectionConfig={vi.fn()}
      onSaveToolServiceConfig={vi.fn()}
      onTestToolService={vi.fn()}
      onRunSchedule={vi.fn()}
      requestNavigation={(navigate) => navigate()}
    />,
  );
}

describe("config tabs keyboard navigation", () => {
  it("keeps the Fleet return path visible across config tabs", () => {
    const onBack = vi.fn();
    renderWorkspace("Back to Fleet", onBack);

    const back = screen.getByRole("button", { name: "← Back to Fleet" });
    fireEvent.click(screen.getByTestId("config-tab-backends"));
    expect(back).toBeVisible();
    fireEvent.click(back);
    expect(onBack).toHaveBeenCalledOnce();
  });

  it("ArrowRight moves selection and focus; Home/End jump; wraps around", () => {
    renderWorkspace();
    const tablist = screen.getByRole("tablist", { name: "Configuration" });
    const tabs = screen.getAllByRole("tab");
    fireEvent.click(tabs[0]);
    expect(tabs[0]).toHaveAttribute("aria-selected", "true");

    fireEvent.keyDown(tablist, { key: "ArrowRight" });
    expect(tabs[1]).toHaveAttribute("aria-selected", "true");
    expect(tabs[0]).toHaveAttribute("tabindex", "-1");

    fireEvent.keyDown(tablist, { key: "End" });
    expect(tabs[tabs.length - 1]).toHaveAttribute("aria-selected", "true");

    fireEvent.keyDown(tablist, { key: "ArrowRight" });
    expect(tabs[0]).toHaveAttribute("aria-selected", "true");

    fireEvent.keyDown(tablist, { key: "ArrowLeft" });
    expect(tabs[tabs.length - 1]).toHaveAttribute("aria-selected", "true");

    fireEvent.keyDown(tablist, { key: "Home" });
    expect(tabs[0]).toHaveAttribute("aria-selected", "true");
  });
});
