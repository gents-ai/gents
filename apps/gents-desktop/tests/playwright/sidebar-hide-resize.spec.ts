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

  test("resizes from its edge, keeps the width, and shows as icons only when dragged narrow", async ({
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

    /* below its minimum it holds, rather than squeezing its rows */
    const edge = (await handle.boundingBox())!;
    const ex = edge.x + edge.width / 2;
    await page.mouse.move(ex, y);
    await page.mouse.down();
    await page.mouse.move(ex - 120, y, { steps: 6 });
    await expect.poll(() => paneX(page)).toBeCloseTo(before - 84, -1);
    /* nearer the rail, it shows as icons only, and comes back when dragged out */
    await page.mouse.move(ex - 220, y, { steps: 6 });
    await expect.poll(() => paneX(page)).toBeLessThan(80);
    await page.mouse.move(ex - 60, y, { steps: 6 });
    await expect.poll(() => paneX(page)).toBeGreaterThan(200);
    await page.mouse.move(ex - 220, y, { steps: 6 });
    await page.mouse.up();
    await expect.poll(() => paneX(page)).toBeLessThan(80);
    /* let go there, it stays icons only, with no panel on hover */
    const saved = async () =>
      page.evaluate(() => JSON.parse(localStorage.getItem("gents-preferences")!).state);
    expect((await saved()).nav).toBe("collapsed");

    /* the rail's edge pulls it out again */
    const rail = (await handle.boundingBox())!;
    const rx = rail.x + rail.width / 2;
    await page.mouse.move(rx, y);
    await page.mouse.down();
    await page.mouse.move(rx + 60, y, { steps: 6 });
    await expect.poll(() => paneX(page)).toBeLessThan(80);
    await page.mouse.move(rx + 200, y, { steps: 6 });
    await expect.poll(() => paneX(page)).toBeGreaterThan(200);
    await page.mouse.up();
    expect((await saved()).nav).toBe("expanded");
  });

  test("its toggle's context menu chooses how it shows", async ({ page }) => {
    await withNav(page, { nav: "expanded" });
    const choose = async (name: string) => {
      await page
        .getByRole("button", { name: /(Hide|Show) sidebar/ })
        .click({ button: "right" });
      await page.getByRole("menuitemradio", { name }).click();
    };
    await choose("Icons only");
    await expect.poll(() => paneX(page)).toBeLessThan(80);
    await choose("Hidden");
    await expect.poll(() => paneX(page)).toBeLessThan(20);
    await choose("Always open");
    await expect.poll(() => paneX(page)).toBeGreaterThan(200);
  });
});
