import { readFileSync } from "node:fs";

import {
  composer,
  expect,
  expectNoPageHorizontalOverflow,
  gotoHarness,
  openChat,
  openConfig,
  openConfigSection,
  test,
  type Page,
} from "./desktopTest";

const mainWindow = (
  JSON.parse(
    readFileSync(new URL("../../src-tauri/tauri.conf.json", import.meta.url), "utf8"),
  ) as { app: { windows: { minWidth: number; minHeight: number }[] } }
).app.windows[0]!;

const HALF_SCREEN = { width: 720, height: 900 };
const MINIMUM = { width: mainWindow.minWidth, height: mainWindow.minHeight };

type Box = { x: number; y: number; width: number; height: number };

async function boxOf(locator: ReturnType<Page["locator"]>) {
  await expect(locator).toBeVisible();
  const box = await locator.boundingBox();
  expect(box).not.toBeNull();
  return box as Box;
}

async function expectInsideViewport(page: Page, locator: ReturnType<Page["locator"]>) {
  const box = await boxOf(locator);
  const { width, height } = page.viewportSize()!;
  expect(box.x).toBeGreaterThanOrEqual(0);
  expect(box.y).toBeGreaterThanOrEqual(0);
  expect(box.x + box.width).toBeLessThanOrEqual(width);
  expect(box.y + box.height).toBeLessThanOrEqual(height);
}

async function expectApart(
  a: ReturnType<Page["locator"]>,
  b: ReturnType<Page["locator"]>,
) {
  const first = await boxOf(a);
  const second = await boxOf(b);
  const overlaps =
    first.x < second.x + second.width &&
    second.x < first.x + first.width &&
    first.y < second.y + second.height &&
    second.y < first.y + first.height;
  expect(overlaps).toBe(false);
}

test.describe("half-screen window", () => {
  test.beforeEach(() => {
    test.skip(
      test.info().project.name !== "chromium-desktop",
      "Sets its own window sizes",
    );
  });

  test("the window can snap to half of a 1440pt display", () => {
    expect(mainWindow.minWidth).toBeLessThanOrEqual(HALF_SCREEN.width);
  });

  for (const viewport of [MINIMUM, HALF_SCREEN]) {
    test(`shell, chat, agents and settings fit ${viewport.width}x${viewport.height}`, async ({
      page,
    }) => {
      await page.setViewportSize(viewport);
      await gotoHarness(page);

      await expect(page.getByTestId("sessions-screen")).toBeVisible();
      const header = page.locator(".app-titlebar");
      await expectInsideViewport(page, page.getByRole("button", { name: "Menu" }));
      await expectApart(
        header.getByRole("link", { name: "Nodes" }).first(),
        header.getByRole("button", { name: /Sync healthy/ }),
      );
      await expectInsideViewport(
        page,
        page.getByRole("button", { name: "New", exact: true }),
      );
      await expectNoPageHorizontalOverflow(page);

      await openChat(page);
      await expectInsideViewport(page, composer(page));
      await expectNoPageHorizontalOverflow(page);

      await page.getByRole("link", { name: "Nodes" }).first().click();
      await expect(page.getByTestId("agents-screen")).toBeVisible();
      await expectInsideViewport(page, page.getByRole("button", { name: "Add node" }));
      await expectNoPageHorizontalOverflow(page);

      await openConfig(page);
      await expect(page.getByRole("combobox", { name: "Section" })).toBeVisible();
      await openConfigSection(page, /^Agents\b/);
      await page
        .getByRole("link", { name: /^Ops\b/ })
        .first()
        .click();
      const name = page.getByRole("textbox", { name: "Display name" });
      await expectInsideViewport(page, name);
      await expectNoPageHorizontalOverflow(page);

      await page.getByRole("button", { name: "Menu" }).click();
      await page.getByRole("link", { name: "Settings" }).click();
      await expectInsideViewport(page, page.getByRole("combobox", { name: "Theme" }));
    });
  }

  for (const viewport of [
    { width: 864, height: 900 },
    { width: 960, height: 900 },
  ]) {
    test(`sessions, a session with its side panel, and agents fit ${viewport.width}x${viewport.height}`, async ({
      page,
    }) => {
      await page.setViewportSize(viewport);
      await gotoHarness(page);
      await expect(page.getByTestId("sessions-screen")).toBeVisible();
      await expectInsideViewport(
        page,
        page.getByRole("button", { name: "New", exact: true }),
      );
      await expectNoPageHorizontalOverflow(page);

      await page
        .getByTestId("sessions-screen")
        .getByRole("link", { name: /introduction-and-greetings/ })
        .click();
      await expectInsideViewport(page, composer(page));
      await page
        .getByRole("button", { name: "More" })
        .filter({ visible: true })
        .first()
        .click();
      await page.getByRole("menuitem", { name: "Trace" }).click();
      const sheet = page.getByRole("dialog", { name: "Side panel" });
      await expect(sheet).toBeVisible();
      await expect(async () => {
        await expectInsideViewport(page, sheet);
      }).toPass({ timeout: 10_000 });
      await expect(page.getByRole("separator", { name: "Resize panel" })).toHaveCount(
        0,
      );
      await page.keyboard.press("Escape");
      await expect(sheet).toHaveCount(0);
      await expectInsideViewport(page, composer(page));
      await expectNoPageHorizontalOverflow(page);

      await page.getByRole("link", { name: "Nodes" }).first().click();
      await expect(page.getByTestId("agents-screen")).toBeVisible();
      await expectInsideViewport(page, page.getByRole("button", { name: "Add node" }));
      await expectNoPageHorizontalOverflow(page);
    });
  }

  test("a narrow window shows the rail in place of an expanded nav", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 900, height: 800 });
    await gotoHarness(page);
    await page.evaluate(() =>
      localStorage.setItem(
        "gents-preferences",
        JSON.stringify({ state: { nav: "expanded" }, version: 1 }),
      ),
    );
    await page.reload();
    await openConfig(page);
    const canvas = await boxOf(page.getByTestId("agent-screen"));
    expect(canvas.width).toBeGreaterThan(800);
    expect((await boxOf(page.getByTestId("pane"))).x).toBeLessThan(100);
    await expect(page.getByRole("combobox", { name: "Section" })).toBeVisible();
    await expectNoPageHorizontalOverflow(page);

    await page.setViewportSize({ width: 1440, height: 900 });
    await expect
      .poll(async () => (await boxOf(page.getByTestId("pane"))).x)
      .toBeGreaterThanOrEqual(300);
    await expect(page.getByRole("link", { name: "New session" }).first()).toBeVisible();
    await expect(page.getByRole("combobox", { name: "Section" })).toBeHidden();
  });
});

test.describe("nav modes", () => {
  test("the mark keeps its place in every nav mode", async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    await gotoHarness(page);
    const mark = page.getByTestId("window-bar").getByRole("link", { name: "Nodes" });
    const places: Record<string, Box> = {};
    for (const nav of ["hover", "collapsed", "expanded"]) {
      await page.evaluate(
        (mode) =>
          localStorage.setItem(
            "gents-preferences",
            JSON.stringify({ state: { nav: mode }, version: 1 }),
          ),
        nav,
      );
      await page.reload();
      await expect(page.getByTestId("app-shell")).toBeVisible();
      places[nav] = await boxOf(mark);
    }
    expect(places.expanded!.x).toBeCloseTo(places.hover!.x, 0);
    expect(places.collapsed!.x).toBeCloseTo(places.hover!.x, 0);
  });
});
