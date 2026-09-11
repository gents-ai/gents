import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { ToolServiceConfigEditor } from "../src/components/config/ToolServiceConfigPanel";
describe("canonical service authoring", () => {
  it("preserves scoped identity and untouched metadata without authoring health", async () => {
    const onSave = vi.fn().mockResolvedValue(undefined);
    const service = {
      agent_did: "owner",
      service_id: " service ",
      display_name: "Service",
      hostname: "localhost",
      mcp_port: 7331,
      send_agent_did: true,
      tags: ["preserve"],
    };
    render(
      <ToolServiceConfigEditor
        agentDid="owner"
        toolService={service}
        saving={false}
        savedStatus={null}
        onSaveToolServiceConfig={onSave}
        onDeleteToolServiceConfig={vi.fn()}
        onDeleted={vi.fn()}
        onSaved={vi.fn()}
        onTestToolService={vi.fn()}
      />,
    );
    expect(screen.queryByTestId("tool-service-status")).not.toBeInTheDocument();
    fireEvent.change(screen.getByTestId("tool-service-display-name"), {
      target: { value: "Edited" },
    });
    fireEvent.click(screen.getByTestId("tool-service-save"));
    await waitFor(() => expect(onSave).toHaveBeenCalledTimes(1));
    expect(onSave.mock.calls[0][0].document).toMatchObject({
      ...service,
      display_name: "Edited",
    });
    expect(onSave.mock.calls[0][0].document).not.toHaveProperty("status");
  });
});
