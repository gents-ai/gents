import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

vi.mock("@/lib/router", () => ({ href: () => "#", navigate: vi.fn() }));

import { TriggersPanel } from "../src/ui/screens/agent/TriggersPanel";
import type { Shell } from "../src/ui/hooks/useShell";
import { deployment } from "./config-panel-wiring/fixtures";

function harness() {
  const api = {
    applyConfigComponents: vi.fn().mockResolvedValue({}),
    saveTriggerConfig: vi.fn().mockResolvedValue({}),
    saveTaskConfig: vi.fn().mockResolvedValue({}),
    saveScheduleConfig: vi.fn().mockResolvedValue({}),
    saveEventSourceConfig: vi.fn().mockResolvedValue({}),
  };
  const shell = {
    api,
    applyConfig: (run: (bridge: typeof api) => Promise<unknown>) => run(api),
  } as unknown as Shell;
  render(<TriggersPanel shell={shell} deployment={deployment} item="trigger-a" />);
  return api;
}

const nothingWritten = (api: ReturnType<typeof harness>) => {
  for (const call of Object.values(api)) expect(call).not.toHaveBeenCalled();
};

describe("new documents from a trigger's fields", () => {
  it("writes nothing when a New dialog opens or is cancelled", async () => {
    const api = harness();
    const user = userEvent.setup();
    for (const name of ["New task", "New schedule"]) {
      await user.click(screen.getByRole("button", { name }));
      const dialog = await screen.findByRole("dialog", { name });
      await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
      await waitFor(() => expect(screen.queryByRole("dialog", { name })).toBeNull());
    }
    nothingWritten(api);
  });

  it("drafts a new task, disabled, and writes it with the trigger on Save", async () => {
    const api = harness();
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "New task" }));
    const dialog = await screen.findByRole("dialog", { name: "New task" });
    await user.type(within(dialog).getByLabelText("Prompt"), "Summarize the day");
    await user.click(within(dialog).getByRole("button", { name: "Add" }));
    nothingWritten(api);

    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(api.applyConfigComponents).toHaveBeenCalledTimes(1));
    const document = api.applyConfigComponents.mock.calls[0][0].document;
    expect(document.tasks).toEqual([
      expect.objectContaining({ prompt_template: "Summarize the day", enabled: false }),
    ]);
    expect(document.triggers).toEqual([
      expect.objectContaining({
        trigger_id: "trigger-a",
        task_id: document.tasks[0].task_id,
      }),
    ]);
    expect(api.saveTaskConfig).not.toHaveBeenCalled();
    expect(api.saveTriggerConfig).not.toHaveBeenCalled();
  });

  it("drafts a new schedule and writes nothing if the trigger is never saved", async () => {
    const api = harness();
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "New schedule" }));
    const dialog = await screen.findByRole("dialog", { name: "New schedule" });
    await user.click(within(dialog).getByRole("button", { name: "Add" }));
    await waitFor(() =>
      expect(screen.queryByRole("dialog", { name: "New schedule" })).toBeNull(),
    );
    /* the trigger's own Cancel, beside its Save */
    const save = screen.getByRole("button", { name: "Save" });
    expect(save).toBeEnabled();
    await user.click(
      within(save.parentElement!).getByRole("button", { name: "Cancel" }),
    );
    expect(save).toBeDisabled();
    nothingWritten(api);
  });

  it("asks which collection a new event source watches, with no default", async () => {
    const api = harness();
    const user = userEvent.setup();
    await user.click(screen.getByRole("combobox", { name: "Source" }));
    await user.click(await screen.findByRole("option", { name: "Event source" }));
    await user.click(screen.getByRole("button", { name: "New event source" }));
    const dialog = await screen.findByRole("dialog", { name: "New event source" });
    expect(within(dialog).getByLabelText("Collection")).toHaveValue("");
    await user.click(within(dialog).getByRole("button", { name: "Add" }));
    expect(
      within(dialog).getByText("Name the collection to watch."),
    ).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    nothingWritten(api);
  });
});
