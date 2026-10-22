import { expect, gotoHarness, test } from "./desktopTest";

test.describe("the app's settings screen", () => {
  test("opens from the rail on its general section and applies a theme change", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 1280, height: 900 });
    await gotoHarness(page);
    /* the rail's link and the nav panel's: both lead there, both mark it */
    const links = page.getByRole("link", { name: "Settings" });
    await links.first().click();
    const screen = page.getByTestId("settings-screen");
    await expect(screen).toHaveAttribute("data-section", "general");
    await expect(links.first()).toHaveAttribute("aria-current", "page");
    await expect(links.last()).toHaveAttribute("aria-current", "page");
    await page.getByRole("combobox", { name: "Theme" }).click();
    await page.getByRole("option", { name: "Dark" }).click();
    await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
    await page.getByRole("combobox", { name: "Theme" }).click();
    await page.getByRole("option", { name: "Light" }).click();
    await expect(page.locator("html")).not.toHaveAttribute("data-theme", "dark");
  });

  test("moves between sections by the sidebar when wide and the select when narrow", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 1280, height: 900 });
    await gotoHarness(page);
    await page.getByRole("link", { name: "Settings" }).first().click();
    /* the side panel shows while the pointer is over the rail */
    await page.mouse.move(900, 600);
    await page.getByRole("link", { name: "Developer" }).click();
    await expect(page.getByTestId("settings-screen")).toHaveAttribute(
      "data-section",
      "developer",
    );
    await page.setViewportSize({ width: 820, height: 900 });
    const section = page.getByRole("combobox", { name: "Section" });
    await expect(section).toBeVisible();
    await section.click();
    await page.getByRole("option", { name: "General" }).click();
    await expect(page.getByTestId("settings-screen")).toHaveAttribute(
      "data-section",
      "general",
    );
    await expect(page.getByRole("combobox", { name: "Theme" })).toBeVisible();
  });
});
