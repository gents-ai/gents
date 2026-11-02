import { expect, gotoHarness, test, type Page } from "./desktopTest";

async function withNav(page: Page, state: Record<string, unknown>) {
  await page.setViewportSize({ width: 1440, height: 900 });
  await gotoHarness(page);
  await page.evaluate(
    (s) =>
      localStorage.setItem(
        "gents-preferences",
        JSON.stringify({ state: s, version: 1 }),
      ),
    state,
  );
  await page.reload();
  await expect(page.getByTestId("app-shell")).toBeVisible();
}

const paneX = async (page: Page) => (await page.getByTestId("pane").boundingBox())!.x;

test.describe("the sidebar", () => {
  test("hides entirely, comes back over the page from the window's edge, and is shown again", async ({
    page,
  }) => {
    await withNav(page, { nav: "expanded" });
    const rail = page.getByTestId("window-bar").getByRole("link", { name: "Nodes" });
    await expect(rail).toBeVisible();
    await page.getByRole("button", { name: "Hide sidebar" }).click();

    /* the page takes the window's width, keeping its own margin */
    await expect.poll(() => paneX(page)).toBeLessThan(16);
    await expect(rail).toBeHidden();
    const peek = page.getByTestId("sidebar-peek");
    await expect(peek).toHaveAttribute("aria-hidden", "true");

    /* resting at the left edge brings it over the page */
    await page.mouse.move(400, 400);
    await page.mouse.move(2, 400);
    await expect(peek).toHaveAttribute("aria-hidden", "false");
    await expect(peek.getByRole("link", { name: "New session" })).toBeVisible();
    expect(await paneX(page)).toBeLessThan(16);
    await page.mouse.move(900, 400);
    await expect(peek).toHaveAttribute("aria-hidden", "true");

    /* kept across a reload, and shown again from the same button */
    await page.reload();
    await expect(page.getByRole("button", { name: "Show sidebar" })).toBeVisible();
    await page.getByRole("button", { name: "Show sidebar" }).click();
    await expect.poll(() => paneX(page)).toBeGreaterThan(280);
    await expect(rail).toBeVisible();
  });

  test("resizes from its edge, keeps the width, and gives way to the rail when dragged narrow", async ({
    page,
  }) => {
    await withNav(page, { nav: "expanded" });
    const handle = page.getByRole("separator", { name: "Resize sidebar" });
    const before = await paneX(page);
    const box = (await handle.boundingBox())!;
    const y = box.y + box.height / 2;
    const x = box.x + box.width / 2;
    await page.mouse.move(x, y);
    await page.mouse.down();
    await page.mouse.move(x + 40, y, { steps: 4 });
    await page.mouse.move(x + 80, y, { steps: 4 });
    await page.mouse.up();
    await expect.poll(() => paneX(page)).toBeCloseTo(before + 80, -1);

    await page.reload();
    await expect.poll(() => paneX(page)).toBeCloseTo(before + 80, -1);

    /* a double-click puts it back */
    await handle.dblclick();
    await expect.poll(() => paneX(page)).toBeCloseTo(before, -1);

    /* past its minimum, nearer the rail: only the rail stays */
    const edge = (await handle.boundingBox())!;
    await page.mouse.move(edge.x + edge.width / 2, y);
    await page.mouse.down();
    await page.mouse.move(edge.x - 120, y, { steps: 6 });
    await page.mouse.move(edge.x - 220, y, { steps: 6 });
    await page.mouse.up();
    await expect.poll(() => paneX(page)).toBeLessThan(80);
    await expect(handle).toHaveCount(0);
  });
});
