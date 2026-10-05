import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { renderIn, testApp } from "./app-fixture";
import { TaskEditor } from "../src/ui/screens/agent/TasksPanel";
import { TriggerEditor } from "../src/ui/screens/agent/TriggersPanel";
import { deployment } from "./config-panel-wiring/fixtures";

vi.mock("@/lib/router", () => ({ href: () => "#", navigate: vi.fn() }));
afterEach(cleanup);

const harness = () => {
  const api = {
    saveTaskConfig: vi.fn().mockResolvedValue({}),
    saveTriggerConfig: vi.fn().mockResolvedValue({}),
  };
  const app = testApp({ api });
  return { api, app };
};

describe("durable delivery editors", () => {
  it("opts a Task into outcomes without adding a Goal budget", async () => {
    const { api, app } = harness();
    renderIn(app, <TaskEditor deployment={deployment} task={deployment.tasks[0]} />);
    await userEvent.click(screen.getByRole("switch", { name: "Emit outcome" }));
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(api.saveTaskConfig).toHaveBeenCalledTimes(1));
    expect(api.saveTaskConfig.mock.calls[0][0].document).toMatchObject({
      emit_outcome: true,
      goal_token_budget: null,
    });
  });

  it("saves queued serial with the existing-session template", async () => {
    const { api, app } = harness();
    const user = userEvent.setup();
    const trigger = {
      ...deployment.triggers[0],
      config: {
        ...deployment.triggers[0].config,
        session_id_template: "{{ doc.lead_session_id }}",
      },
    };
    renderIn(app, <TriggerEditor deployment={deployment} trigger={trigger} />);
    expect(screen.getByLabelText("Existing session template")).toHaveValue(
      "{{ doc.lead_session_id }}",
    );
    await user.click(screen.getByRole("combobox", { name: "Concurrency" }));
    expect(
      await screen.findByRole("option", { name: "Serial (skip when busy)" }),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("option", { name: "Queued serial" }));
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(api.saveTriggerConfig).toHaveBeenCalledTimes(1));
    expect(api.saveTriggerConfig.mock.calls[0][0].document).toMatchObject({
      concurrency: "queued_serial",
      session_id_template: "{{ doc.lead_session_id }}",
    });
  });
});
