import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { GeneralPanel } from "../src/ui/screens/settings/GeneralPanel";
import { renderIn, testApp } from "./app-fixture";

describe("the viewer's preferences", () => {
  it("apply the theme to the document as the general settings change it", async () => {
    const user = userEvent.setup();
    renderIn(testApp(), <GeneralPanel />);
    await user.click(screen.getByRole("combobox", { name: "Theme" }));
    await user.click(await screen.findByRole("option", { name: "Dark" }));
    expect(document.documentElement.dataset.theme).toBe("dark");
    expect(screen.getByRole("combobox", { name: "Theme" })).toHaveTextContent("Dark");
  });
});
