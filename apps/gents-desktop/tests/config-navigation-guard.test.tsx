import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { ConfirmDialog } from "@source-inc/gents-desktop-ui";
import { describe, expect, it, vi } from "vitest";

import { ConfigWorkspace } from "../src/components/ConfigWorkspace";
import { useConfigNavigationController } from "../src/components/config/ConfigNavigationGuard";
import {
  bootstrap,
  deployment,
  workspaceHandlers,
} from "./config-panel-wiring/fixtures";

function makeHandlers() {
  return {
    ...workspaceHandlers(),
    onDeleteSkillConfig: vi.fn(),
    onDeleteTaskConfig: vi.fn(),
    onDeleteScheduleConfig: vi.fn(),
    onDeleteEventTriggerConfig: vi.fn(),
    onDeleteBackendConfig: vi.fn(),
    onDeleteInferenceProfileConfig: vi.fn(),
    onDeleteToolSelectionConfig: vi.fn(),
    onDeleteToolServiceConfig: vi.fn(),
    onDeleteBehaviorConfig: vi.fn(),
  };
}

function GuardedWorkspace({ handlers }: { handlers: ReturnType<typeof makeHandlers> }) {
  const navigation = useConfigNavigationController();
  return (
    <>
      <ConfigWorkspace
        bootstrap={bootstrap}
        selectedBehaviorId="default"
        selectedDeployment={deployment}
        saving={false}
        runningTask={false}
        onDirtyChange={navigation.reportDirty}
        requestNavigation={navigation.requestNavigation}
        {...handlers}
        onBack={() => navigation.requestNavigation(handlers.onBack)}
      />
      <ConfirmDialog
        cancelLabel="Keep editing"
        confirmLabel="Discard changes"
        danger
        message="This configuration has unsaved changes. Discard them and continue?"
        onCancel={navigation.cancelDiscard}
        onConfirm={navigation.confirmDiscard}
        open={navigation.confirmingDiscard}
        title="Discard unsaved changes?"
      />
    </>
  );
}

function renderWorkspace() {
  const handlers = makeHandlers();

  render(<GuardedWorkspace handlers={handlers} />);
  return handlers;
}

function editBehaviorPrompt(value: string) {
  fireEvent.change(screen.getByTestId("behavior-system-prompt"), {
    target: { value },
  });
  expect(screen.getByTestId("unsaved-chip")).toBeInTheDocument();
}

describe("config navigation guard", () => {
  it("keeps an edited form mounted until tab navigation is confirmed", () => {
    renderWorkspace();
    editBehaviorPrompt("keep this tab edit");

    fireEvent.click(screen.getByTestId("config-tab-backends"));

    expect(screen.getByTestId("confirm-dialog")).toBeInTheDocument();
    expect(screen.getByTestId("config-tab-behavior")).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(screen.getByTestId("behavior-system-prompt")).toHaveValue(
      "keep this tab edit",
    );

    fireEvent.click(screen.getByTestId("confirm-dialog-cancel"));
    expect(screen.queryByTestId("confirm-dialog")).not.toBeInTheDocument();
    expect(screen.getByTestId("behavior-system-prompt")).toHaveValue(
      "keep this tab edit",
    );

    fireEvent.click(screen.getByTestId("config-tab-backends"));
    fireEvent.click(screen.getByTestId("confirm-dialog-confirm"));

    expect(screen.getByTestId("config-tab-backends")).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(screen.getByTestId("backend-endpoint")).toBeInTheDocument();
  });

  it("guards document selection and hydrates the next document after discard", async () => {
    renderWorkspace();
    editBehaviorPrompt("keep this document edit");

    fireEvent.click(screen.getByTestId("config-behavior-ops"));

    expect(screen.getByTestId("confirm-dialog")).toBeInTheDocument();
    expect(screen.getByTestId("behavior-system-prompt")).toHaveValue(
      "keep this document edit",
    );

    fireEvent.click(screen.getByTestId("confirm-dialog-confirm"));

    await waitFor(() =>
      expect(screen.getByTestId("behavior-system-prompt")).toHaveValue(
        "You are the ops behavior.",
      ),
    );
  });

  it("guards returning to chat and protects browser-level navigation", () => {
    const handlers = renderWorkspace();
    editBehaviorPrompt("keep this back-navigation edit");

    const beforeUnload = new Event("beforeunload", {
      cancelable: true,
    }) as BeforeUnloadEvent;
    expect(window.dispatchEvent(beforeUnload)).toBe(false);
    expect(beforeUnload.defaultPrevented).toBe(true);

    fireEvent.click(screen.getByTestId("config-back-tab"));
    expect(handlers.onBack).not.toHaveBeenCalled();

    fireEvent.click(screen.getByTestId("confirm-dialog-confirm"));
    expect(handlers.onBack).toHaveBeenCalledOnce();
  });
});
