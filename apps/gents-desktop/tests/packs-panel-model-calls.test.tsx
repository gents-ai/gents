import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { PacksPanel } from "../src/ui/screens/agent/PacksPanel";

const bridge = vi.hoisted(() => ({ call: vi.fn() }));
vi.mock("../src/ui/screens/agent/bridgeCall", () => ({
  call: bridge.call,
  message: (error: unknown) => String(error),
}));
vi.mock("sonner", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

const profiles = [
  {
    profile_id: "chandra",
    display_name: "Chandra",
    model_name: "chandra",
    usable: true,
  },
  { profile_id: "down", display_name: "Down", model_name: "down", usable: false },
];

function respond(plugins: { plugin: string; slot: string; profile: string | null }[]) {
  bridge.call.mockImplementation(async (command: string) => {
    if (command === "desktop_pack_installed") return { packs: [] };
    if (command === "desktop_pack_whoami") throw new Error("signed out");
    if (command === "desktop_pack_plugin_slots") return { plugins, profiles };
    return {};
  });
}

describe("PacksPanel model calls", () => {
  beforeEach(() => bridge.call.mockReset());

  it("shows nothing when no installed plugin can call a model", async () => {
    respond([]);
    render(<PacksPanel />);
    await screen.findByText("No packs installed");
    expect(screen.queryByText("Model calls")).toBeNull();
  });

  it("leaves a plugin's slot unset and binds it to a usable profile later", async () => {
    const user = userEvent.setup();
    respond([{ plugin: "gents/ocr", slot: "remote_ocr", profile: null }]);
    render(<PacksPanel />);
    const select = await screen.findByRole("combobox", { name: "gents/ocr" });
    expect(select).toHaveTextContent("Not set");
    await user.click(select);
    expect(screen.queryByRole("option", { name: "Down" })).toBeNull();
    await user.click(await screen.findByRole("option", { name: "Chandra" }));
    await waitFor(() =>
      expect(bridge.call).toHaveBeenCalledWith("desktop_pack_plugin_bind", {
        request: { plugin: "gents/ocr", profile: "chandra" },
      }),
    );
  });

  it("unbinds a bound slot by choosing Not set", async () => {
    const user = userEvent.setup();
    respond([{ plugin: "gents/ocr", slot: "remote_ocr", profile: "chandra" }]);
    render(<PacksPanel />);
    const select = await screen.findByRole("combobox", { name: "gents/ocr" });
    expect(select).toHaveTextContent("Chandra");
    await user.click(select);
    await user.click(await screen.findByRole("option", { name: "Not set" }));
    await waitFor(() =>
      expect(bridge.call).toHaveBeenCalledWith("desktop_pack_plugin_bind", {
        request: { plugin: "gents/ocr", profile: null },
      }),
    );
  });
});
