import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { SettingsMenu } from "../src/ui/app/SettingsMenu";
import { renderIn, testApp } from "./app-fixture";

describe("the viewer's preferences", () => {
  it("show the same theme in every settings menu once one changes it", async () => {
    const user = userEvent.setup();
    renderIn(
      testApp(),
      <>
        <SettingsMenu variant="rail" showNav />
        <SettingsMenu variant="row" showNav />
      </>,
    );
    const [rail, row] = screen.getAllByRole("button", { name: "Settings" });
    await user.click(rail!);
    await user.click(await screen.findByRole("menuitemradio", { name: "Dark" }));
    await user.keyboard("{Escape}");

    await user.click(row!);
    expect(await screen.findByRole("menuitemradio", { name: "Dark" })).toHaveAttribute(
      "aria-checked",
      "true",
    );
    expect(document.documentElement.dataset.theme).toBe("dark");
  });
});
