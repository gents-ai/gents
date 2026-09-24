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

/* the native window's configured minimum, so the layout is checked at the
   narrowest size a person can actually make the window */
const mainWindow = (
  JSON.parse(
    readFileSync(new URL("../../src-tauri/tauri.conf.json", import.meta.url), "utf8"),
  ) as { app: { windows: { minWidth: number; minHeight: number }[] } }
).app.windows[0]!;

/* half of a 1440pt-wide display, the common MacBook size a window snaps to */
const HALF_SCREEN = { width: 720, height: 900 };
const MINIMUM = { width: mainWindow.minWidth, height: mainWindow.minHeight };

type Box = { x: number; y: number; width: number; height: number };

async function boxOf(page: Page, locator: ReturnType<Page["locator"]>) {
  await expect(locator).toBeVisible();
  const box = await locator.boundingBox();
  expect(box).not.toBeNull();
  return box as Box;
}

async function expectInsideViewport(page: Page, locator: ReturnType<Page["locator"]>) {
  const box = await boxOf(page, locator);
  const { width, height } = page.viewportSize()!;
  expect(box.x).toBeGreaterThanOrEqual(0);
  expect(box.y).toBeGreaterThanOrEqual(0);
  expect(box.x + box.width).toBeLessThanOrEqual(width);
  expect(box.y + box.height).toBeLessThanOrEqual(height);
}

async function expectApart(
  page: Page,
  a: ReturnType<Page["locator"]>,
  b: ReturnType<Page["locator"]>,
) {
  const first = await boxOf(page, a);
  const second = await boxOf(page, b);
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

      /* shell: the rail collapses into the menu; the header items stay apart */
      await expect(page.getByTestId("sessions-screen")).toBeVisible();
      const header = page.locator(".app-titlebar");
      await expectInsideViewport(page, page.getByRole("button", { name: "Menu" }));
      await expectApart(
        page,
        header.getByLabel("breadcrumb"),
        header.getByRole("button", { name: /Sync healthy/ }),
      );
      await expectInsideViewport(
        page,
        page.getByRole("button", { name: "New", exact: true }),
      );
      await expectNoPageHorizontalOverflow(page);

      /* chat: the composer stays on the canvas */
      await openChat(page);
      await expectInsideViewport(page, composer(page));
      await expectNoPageHorizontalOverflow(page);

      /* agents */
      await page.getByLabel("breadcrumb").getByRole("link", { name: "Agents" }).click();
      await expect(page.getByTestId("agents-screen")).toBeVisible();
      await expectInsideViewport(page, page.getByRole("button", { name: "Add agent" }));
      await expectNoPageHorizontalOverflow(page);

      /* configuration: the section list becomes a picker and editors stack */
      await openConfig(page);
      await expect(page.getByRole("combobox", { name: "Section" })).toBeVisible();
      await openConfigSection(page, /^Behaviors\b/);
      await page
        .getByRole("link", { name: /^Ops\b/ })
        .first()
        .click();
      const name = page.getByRole("textbox", { name: "Display name" });
      await expectInsideViewport(page, name);
      await expectNoPageHorizontalOverflow(page);

      /* settings: the menu and its theme choices stay on screen */
      await page.getByRole("button", { name: "Menu" }).click();
      await page.getByRole("button", { name: "Settings" }).click();
      await expectInsideViewport(
        page,
        page.getByRole("menuitemradio", { name: "Dark" }),
      );
    });
  }

  /* half of 1728pt and 1920pt displays: the rail stays, panes overlay */
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
        .getByRole("button", { name: "Open side panel" })
        .filter({ visible: true })
        .first()
        .click();
      /* the side panel opens as a sheet over the transcript, not a column */
      const sheet = page.getByRole("dialog", { name: "Side panel" });
      await expect(sheet).toBeVisible();
      await sheet.evaluate((element) =>
        element.getAnimations({ subtree: true }).forEach((a) => a.finish()),
      );
      await expectInsideViewport(page, sheet);
      await expect(page.getByRole("separator", { name: "Resize trace" })).toHaveCount(
        0,
      );
      await page.keyboard.press("Escape");
      await expect(sheet).toHaveCount(0);
      await expectInsideViewport(page, composer(page));
      await expectNoPageHorizontalOverflow(page);

      await page.getByLabel("breadcrumb").getByRole("link", { name: "Agents" }).click();
      await expect(page.getByTestId("agents-screen")).toBeVisible();
      await expectInsideViewport(page, page.getByRole("button", { name: "Add agent" }));
      await expectNoPageHorizontalOverflow(page);
    });
  }

  test("a narrow window shows the rail in place of an expanded nav", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 900, height: 800 });
    await gotoHarness(page);
    await page.evaluate(() => localStorage.setItem("gents-prototype-nav", "expanded"));
    await page.reload();
    await openConfig(page);
    /* the canvas keeps the room the expanded nav would take */
    const canvas = await boxOf(page, page.getByTestId("agent-screen"));
    expect(canvas.width).toBeGreaterThan(800);
    await expect(page.getByRole("combobox", { name: "Section" })).toBeVisible();
    await expectNoPageHorizontalOverflow(page);

    await page.setViewportSize({ width: 1440, height: 900 });
    await expect(page.getByRole("link", { name: "New session" }).first()).toBeVisible();
    await expect(page.getByRole("combobox", { name: "Section" })).toBeHidden();
  });
});
