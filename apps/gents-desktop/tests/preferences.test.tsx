import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { GeneralPanel } from "../src/ui/screens/settings/GeneralPanel";
import { renderIn, testApp } from "./app-fixture";
import * as nativeShell from "../src/lib/nativeShell";

afterEach(() => vi.restoreAllMocks());

describe("the viewer's preferences", () => {
  it("apply the theme to the document as the general settings change it", async () => {
    const user = userEvent.setup();
    renderIn(testApp(), <GeneralPanel />);
    await user.click(screen.getByRole("combobox", { name: "Theme" }));
    await user.click(await screen.findByRole("option", { name: "Dark" }));
    expect(document.documentElement.dataset.theme).toBe("dark");
    expect(screen.getByRole("combobox", { name: "Theme" })).toHaveTextContent("Dark");
  });

  it("allows a saved opt-in to be turned off when telemetry is unconfigured", async () => {
    vi.spyOn(nativeShell, "productTelemetryStatus").mockResolvedValue({
      configured: false,
      enabled: true,
    });
    const save = vi
      .spyOn(nativeShell, "setProductTelemetry")
      .mockResolvedValue({ configured: false, enabled: false });
    const user = userEvent.setup();
    renderIn(testApp(), <GeneralPanel />);
    await user.click(await screen.findByRole("combobox", { name: "Usage telemetry" }));
    await user.click(await screen.findByRole("option", { name: "Off" }));
    await waitFor(() => expect(save).toHaveBeenCalledWith(false));
    expect(
      await screen.findByText("Not set up in this build, so nothing is sent."),
    ).toBeVisible();
  });

  it("does not offer opt-in without a configured endpoint", async () => {
    vi.spyOn(nativeShell, "productTelemetryStatus").mockResolvedValue({
      configured: false,
      enabled: false,
    });
    renderIn(testApp(), <GeneralPanel />);
    expect(
      await screen.findByText("Not set up in this build, so nothing is sent."),
    ).toBeVisible();
    expect(screen.queryByRole("combobox", { name: "Usage telemetry" })).toBeNull();
  });

  it("shows a status that can't be read as the setting's row, not loose text", async () => {
    vi.spyOn(nativeShell, "productTelemetryStatus").mockRejectedValue(
      new Error("denied"),
    );
    renderIn(testApp(), <GeneralPanel />);
    const status = await screen.findByText("Couldn't read this setting.");
    expect(status.closest("[data-slot=field-row]")).toHaveTextContent(
      "Usage telemetry",
    );
    expect(screen.queryByText(/Couldn't save/)).toBeNull();
  });

  it("says a change didn't save on the setting itself", async () => {
    vi.spyOn(nativeShell, "productTelemetryStatus").mockResolvedValue({
      configured: true,
      enabled: false,
    });
    vi.spyOn(nativeShell, "setProductTelemetry").mockRejectedValue(new Error("denied"));
    const user = userEvent.setup();
    renderIn(testApp(), <GeneralPanel />);
    await user.click(await screen.findByRole("combobox", { name: "Usage telemetry" }));
    await user.click(await screen.findByRole("option", { name: "On" }));
    const error = await screen.findByText(
      "Couldn't save the change. It may not last after a restart.",
    );
    expect(error.closest("[data-slot=field-row]")).toContainElement(
      screen.getByRole("combobox", { name: "Usage telemetry" }),
    );
  });

  it("prevents overlapping consent changes while a save is pending", async () => {
    vi.spyOn(nativeShell, "productTelemetryStatus").mockResolvedValue({
      configured: true,
      enabled: false,
    });
    let complete!: (value: nativeShell.ProductTelemetryStatus) => void;
    const save = vi.spyOn(nativeShell, "setProductTelemetry").mockReturnValue(
      new Promise((resolve) => {
        complete = resolve;
      }),
    );
    const user = userEvent.setup();
    renderIn(testApp(), <GeneralPanel />);
    await user.click(await screen.findByRole("combobox", { name: "Usage telemetry" }));
    await user.click(await screen.findByRole("option", { name: "On" }));
    expect(save).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("combobox", { name: "Usage telemetry" })).toBeDisabled();
    complete({ configured: true, enabled: true });
    await waitFor(() =>
      expect(
        screen.getByRole("combobox", { name: "Usage telemetry" }),
      ).toHaveTextContent("On"),
    );
  });
});
