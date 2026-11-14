import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { PluginWindowWaiting } from "../src/ui/app/PluginWindowWaiting";

describe("a plugin's window while the app starts", () => {
  it("says which page it is opening, and offers no setup", () => {
    render(<PluginWindowWaiting phase="starting-client" title="Board" />);
    expect(screen.getByRole("status")).toHaveTextContent("Opening Board…");
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("waits without a name when the window has none", () => {
    render(<PluginWindowWaiting phase="loading-configuration" title={null} />);
    expect(screen.getByRole("status")).toHaveTextContent("Opening…");
  });

  it("points to the main window when starting fails", () => {
    render(<PluginWindowWaiting phase="client-error" title="Board" />);
    expect(screen.getByTestId("plugin-window-waiting")).toHaveAttribute(
      "data-failed",
      "true",
    );
    expect(screen.getByRole("heading")).toHaveTextContent("This page can’t open yet");
    expect(screen.getByText(/main window says why/)).toBeVisible();
    expect(screen.queryByRole("status")).toBeNull();
    expect(screen.queryByRole("button")).toBeNull();
  });
});
