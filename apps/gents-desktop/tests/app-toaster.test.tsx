import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { toast } from "sonner";
import { AppToaster } from "../src/ui/app/AppToaster";
import { preferences, resetPreferences } from "../src/ui/preferences";

afterEach(() => {
  cleanup();
  resetPreferences();
});

describe("the app's notices", () => {
  it("are drawn in the theme the person chose", async () => {
    preferences.setTheme("dark");
    render(<AppToaster />);
    act(() => {
      toast("Someone asked to join", { description: "Approve or deny it" });
    });
    await screen.findByText("Approve or deny it");
    expect(document.querySelector("[data-sonner-toaster]")).toHaveAttribute(
      "data-sonner-theme",
      "dark",
    );
  });
});
