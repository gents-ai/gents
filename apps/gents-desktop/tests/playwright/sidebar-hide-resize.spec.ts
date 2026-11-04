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

/* a view transition draws over the page while it runs, so a press waits
   for it to end */
const settled = (page: Page) =>
  page.waitForFunction(
    () =>
      !document
        .getAnimations()
        .some((a) =>
          (a.effect as KeyframeEffect | null)?.pseudoElement?.startsWith(
            "::view-transition",
          ),
        ),
  );

const paneX = async (page: Page) => (await page.getByTestId("pane").boundingBox())!.x;

test.describe("the sidebar", () => {
  /* it sets its own 1440×900 window, so the laptop and narrow projects
     would only repeat the desktop Chromium run */
  test.beforeEach(({}, info) => {
    test.skip(
      info.project.name === "chromium-laptop" ||
        info.project.name === "chromium-narrow",
      "sets its own desktop window",
    );
  });
  test("hides entirely, comes back over the page from the window's edge, and is shown again", async ({
    page,
  }) => {
    await withNav(page, { nav: "expanded" });
    const rail = page.getByTestId("window-bar").getByRole("link", { name: "Nodes" });
    await expect(rail).toBeVisible();
    await page
      .getByRole("button", { name: "Collapse sidebar" })
      .click({ button: "right" });
    await page.getByRole("menuitemradio", { name: "Hidden" }).click();

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

    /* kept across a reload, and shown again, always open, from its toggle */
    await page.reload();
    await expect(page.getByRole("button", { name: "Expand sidebar" })).toBeVisible();
    await page.getByRole("button", { name: "Expand sidebar" }).click();
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
        .getByRole("button", { name: /(Collapse|Expand) sidebar/ })
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

  test("its toggle collapses and expands on a click, and a held press opens its modes", async ({
    page,
  }) => {
    await withNav(page, { nav: "expanded" });
    const toggle = page.getByRole("button", { name: /(Collapse|Expand) sidebar/ });
    await page.getByRole("button", { name: "Collapse sidebar" }).click();
    await expect.poll(() => paneX(page)).toBeLessThan(80);
    await settled(page);
    await page.getByRole("button", { name: "Expand sidebar" }).click();
    await expect.poll(() => paneX(page)).toBeGreaterThan(200);
    await settled(page);

    /* held, it opens the menu of modes without toggling */
    const box = (await toggle.boundingBox())!;
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await page.mouse.down();
    const hidden = page.getByRole("menuitemradio", { name: "Hidden" });
    await expect(hidden).toBeVisible();
    await page.mouse.up();
    await expect(hidden).toBeVisible();
    expect(await paneX(page)).toBeGreaterThan(200);
    await hidden.click();
    await expect.poll(() => paneX(page)).toBeLessThan(20);
    await settled(page);

    /* held and dragged onto a choice, letting go chooses it */
    const shown = (await toggle.boundingBox())!;
    await page.mouse.move(shown.x + shown.width / 2, shown.y + shown.height / 2);
    await page.mouse.down();
    const icons = page.getByRole("menuitemradio", { name: "Icons only" });
    await expect(icons).toBeVisible();
    const item = (await icons.boundingBox())!;
    await page.mouse.move(item.x + item.width / 2, item.y + item.height / 2, {
      steps: 5,
    });
    await page.mouse.up();
    await expect(icons).toBeHidden();
    await expect.poll(() => paneX(page)).toBeLessThan(80);
    expect(await paneX(page)).toBeGreaterThan(20);
  });

  test.describe("read right to left", () => {
    const rtl = (page: Page) =>
      page.evaluate(() => {
        document.documentElement.dir = "rtl";
      });
    const box = async (page: Page, testId: string) =>
      (await page.getByTestId(testId).boundingBox())!;

    test("opens its hover panel on screen, beside the rail at the right", async ({
      page,
    }) => {
      await withNav(page, { nav: "hover" });
      await rtl(page);
      const rail = page.locator("nav").first();
      const at = (await rail.boundingBox())!;
      expect(at.x).toBeGreaterThan(1440 / 2);
      await page.mouse.move(at.x + at.width / 2, at.y + at.height / 2);
      const panel = page.locator("[data-ground=raised]");
      await expect(panel).toHaveAttribute("aria-hidden", "false");
      const drawn = (await panel.boundingBox())!;
      expect(drawn.x).toBeGreaterThanOrEqual(0);
      expect(drawn.x + drawn.width).toBeLessThanOrEqual(1440);
      expect(drawn.x + drawn.width).toBeGreaterThan(at.x);
    });

    test("widens as its edge is dragged toward the window's left", async ({ page }) => {
      await withNav(page, { nav: "expanded" });
      await rtl(page);
      const pane = await box(page, "pane");
      const handle = page.getByRole("separator", { name: "Resize sidebar" });
      const edge = (await handle.boundingBox())!;
      const y = edge.y + edge.height / 2;
      expect(edge.x).toBeGreaterThan(pane.x + pane.width - 20);
      await page.mouse.move(edge.x + edge.width / 2, y);
      await page.mouse.down();
      await page.mouse.move(edge.x + edge.width / 2 - 60, y, { steps: 6 });
      await page.mouse.up();
      await expect
        .poll(async () => {
          const now = await box(page, "pane");
          return now.x + now.width;
        })
        .toBeLessThan(pane.x + pane.width - 40);
    });

    test("peeks in from the window's right edge when hidden", async ({ page }) => {
      await withNav(page, { navHidden: true });
      await rtl(page);
      const edge = await box(page, "sidebar-edge");
      expect(edge.x + edge.width).toBeGreaterThan(1440 - 2);
      await page.mouse.move(edge.x + edge.width / 2, edge.y + 200);
      const peek = page.getByTestId("sidebar-peek");
      await expect(peek).toHaveAttribute("aria-hidden", "false");
      await expect
        .poll(async () => {
          const drawn = (await peek.boundingBox())!;
          return Math.round(drawn.x + drawn.width);
        })
        .toBe(1440);
    });
  });
});
