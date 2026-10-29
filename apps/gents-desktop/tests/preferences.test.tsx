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
    expect(await screen.findByText("Usage telemetry is not configured.")).toBeVisible();
  });

  it("does not offer opt-in without a configured endpoint", async () => {
    vi.spyOn(nativeShell, "productTelemetryStatus").mockResolvedValue({
      configured: false,
      enabled: false,
    });
    renderIn(testApp(), <GeneralPanel />);
    expect(await screen.findByText("Usage telemetry is not configured.")).toBeVisible();
    expect(screen.queryByRole("combobox", { name: "Usage telemetry" })).toBeNull();
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
