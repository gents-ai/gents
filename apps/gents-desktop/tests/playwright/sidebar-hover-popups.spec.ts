import type { Locator } from "@playwright/test";
import { expect, gotoHarness, test, type Page } from "./desktopTest";

async function hoverMode(page: Page) {
  await page.setViewportSize({ width: 1440, height: 900 });
  await gotoHarness(page, "active-nodes");
  await page.evaluate(() =>
    localStorage.setItem(
      "gents-preferences",
      JSON.stringify({ state: { nav: "hover" }, version: 1 }),
    ),
  );
  await page.reload();
  await expect(page.getByTestId("app-shell")).toBeVisible();
}

const centre = async (locator: Locator) => {
  const box = (await locator.boundingBox())!;
  return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
};

/* the panel and its node switcher, opened from the rail by the pointer */
async function openSwitcher(page: Page) {
  const panel = page.locator("[data-ground=raised]");
  const rail = await centre(page.locator("nav").first());
  await page.mouse.move(rail.x, rail.y);
  await expect(panel).toHaveAttribute("aria-hidden", "false");
  const button = panel.getByRole("button", { name: "Switch node" });
  const at = await centre(button);
  await page.mouse.move(at.x, at.y, { steps: 4 });
  await page.mouse.click(at.x, at.y);
  const menu = page.getByRole("menu");
  await expect(menu).toBeVisible();
  return { panel, menu };
}

test.describe("the hover panel and what opens from it", () => {
  /* it sets its own 1440×900 window, so the laptop and narrow projects
     would only repeat the desktop Chromium run */
  test.beforeEach(({}, info) => {
    test.skip(
      info.project.name === "chromium-laptop" ||
        info.project.name === "chromium-narrow",
      "sets its own desktop window",
    );
  });
  test("stay open together while the pointer is on either, and close together", async ({
    page,
  }) => {
    await hoverMode(page);
    const { panel, menu } = await openSwitcher(page);
    const item = await centre(menu.getByRole("menuitemradio").last());
    await page.mouse.move(item.x, item.y, { steps: 8 });
    await page.waitForTimeout(400);
    await expect(panel).toHaveAttribute("aria-hidden", "false");
    await expect(menu).toBeVisible();
    await page.mouse.move(1100, 500, { steps: 8 });
    await expect(menu).toBeHidden();
    await expect(panel).toHaveAttribute("aria-hidden", "true");
  });

  test("closes once a choice from its menu is made and the pointer moves away", async ({
    page,
  }) => {
    await hoverMode(page);
    const { panel, menu } = await openSwitcher(page);
    const other = menu.getByRole("menuitemradio", { checked: false }).first();
    const item = await centre(other);
    await page.mouse.move(item.x, item.y, { steps: 8 });
    await page.mouse.click(item.x, item.y);
    await expect(menu).toBeHidden();
    /* the menu went from under the pointer, which now rests outside the panel */
    await page.mouse.move(item.x + 40, item.y + 10, { steps: 4 });
    await expect(panel).toHaveAttribute("aria-hidden", "true");
  });

  test("close together when the pointer goes back to the panel and leaves it", async ({
    page,
  }) => {
    await hoverMode(page);
    const { panel, menu } = await openSwitcher(page);
    const row = await centre(panel.getByRole("link", { name: /New session/ }));
    await page.mouse.move(row.x, row.y, { steps: 8 });
    await page.waitForTimeout(300);
    await expect(panel).toHaveAttribute("aria-hidden", "false");
    await page.mouse.move(1100, row.y, { steps: 8 });
    await expect(panel).toHaveAttribute("aria-hidden", "true");
    await expect(menu).toBeHidden();
  });

  test("close together from a hidden sidebar's peek", async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    await gotoHarness(page, "active-nodes");
    await page.evaluate(() =>
      localStorage.setItem(
        "gents-preferences",
        JSON.stringify({ state: { nav: "expanded", navHidden: true }, version: 1 }),
      ),
    );
    await page.reload();
    const peek = page.getByTestId("sidebar-peek");
    await page.mouse.move(400, 400);
    await page.mouse.move(2, 400);
    await expect(peek).toHaveAttribute("aria-hidden", "false");
    /* it slides in; its switcher is where it rests */
    await page.waitForTimeout(300);
    const at = await centre(peek.getByRole("button", { name: "Switch node" }));
    await page.mouse.move(at.x, at.y, { steps: 4 });
    await page.mouse.click(at.x, at.y);
    const menu = page.getByRole("menu");
    await expect(menu).toBeVisible();
    await page.mouse.move(1100, 500, { steps: 8 });
    await expect(peek).toHaveAttribute("aria-hidden", "true");
    await expect(menu).toBeHidden();
  });
});
