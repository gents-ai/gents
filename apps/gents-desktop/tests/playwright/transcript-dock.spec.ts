import type { Page } from "@playwright/test";
import { expect, gotoHarness, seedLocalStorage, test } from "./desktopTest";

async function widthsDuring(page: Page, act: () => Promise<void>) {
  await page.evaluate(() => {
    const widths: { pane: number; dock: number }[] = [];
    const surface = () =>
      document.querySelector<HTMLElement>(
        '[data-testid="dock-cell"] .h-full > .h-full',
      );
    const tick = () => {
      widths.push({
        pane: Math.round(
          document.querySelector('[data-testid="pane"] > main')!.getBoundingClientRect()
            .width,
        ),
        dock:
          (document.querySelector('[data-testid="dock-cell"]')?.getBoundingClientRect()
            .width ?? 0) > 16
            ? Math.round(surface()?.getBoundingClientRect().width ?? 0)
            : 0,
      });
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
    Object.assign(window, { __widths: widths });
  });
  await act();
  await page.waitForTimeout(1500);
  return page.evaluate(
    () =>
      (window as unknown as { __widths: { pane: number; dock: number }[] }).__widths,
  );
}

const distinct = (values: number[]) => [...new Set(values)];

test("opening and closing the dock lays the pane out once", async ({
  page,
}, testInfo) => {
  test.skip(
    !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
    "one layout per engine",
  );
  await page.setViewportSize({ width: 1500, height: 900 });
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  await page.getByTestId("transcript-panel").getByText("stream-start").last().waitFor();

  const opening = await widthsDuring(page, async () => {
    await page.getByRole("button", { name: "More" }).first().click();
    await page.getByRole("menuitem", { name: "Started sessions" }).click();
  });
  expect(distinct(opening.map((w) => w.pane))).toHaveLength(2);
  expect(distinct(opening.map((w) => w.dock).filter((w) => w > 0))).toHaveLength(1);

  const closing = await widthsDuring(page, async () => {
    await page.getByRole("button", { name: "Hide panel" }).click();
  });
  expect(distinct(closing.map((w) => w.pane))).toHaveLength(2);
  expect(distinct(closing.map((w) => w.dock).filter((w) => w > 0))).toHaveLength(1);
});

test("dragging the divider lays the pane out once, on release", async ({
  page,
}, testInfo) => {
  test.skip(
    !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
    "one layout per engine",
  );
  await page.setViewportSize({ width: 1500, height: 900 });
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  await page.getByTestId("transcript-panel").getByText("stream-start").last().waitFor();
  await page.getByRole("button", { name: "More" }).first().click();
  await page.getByRole("menuitem", { name: "Started sessions" }).click();
  await page.waitForTimeout(1200);
  const handle = page.getByRole("separator", { name: "Resize panel" });
  const box = (await handle.boundingBox())!;
  const y = box.y + box.height / 2;
  let during: number[] = [];
  const widths = await widthsDuring(page, async () => {
    await page.mouse.move(box.x + box.width / 2, y);
    await page.mouse.down();
    for (let i = 1; i <= 12; i += 1) await page.mouse.move(box.x - i * 10, y);
    during = await page.evaluate(() =>
      (window as unknown as { __widths: { pane: number }[] }).__widths.map(
        (w) => w.pane,
      ),
    );
    await page.mouse.up();
  });
  expect(distinct(during)).toHaveLength(1);
  expect(distinct(widths.map((w) => w.pane))).toHaveLength(2);
});

test("nothing over the transcript is a size container", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== "webkit-desktop", "one engine");
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  await page.getByTestId("transcript-panel").getByText("stream-start").last().waitFor();
  const containers = await page.getByTestId("transcript-panel").evaluate((panel) => {
    const found: string[] = [];
    for (let el: Element | null = panel; el; el = el.parentElement)
      if (getComputedStyle(el).containerType !== "normal")
        found.push(`${el.tagName.toLowerCase()}.${el.className}`);
    return found;
  });
  expect(containers).toEqual([]);
});

async function tabEdgeDuring(page: Page, act: () => Promise<void>) {
  await page.evaluate(() => {
    const lefts: number[] = [];
    const tick = () => {
      const tab = document.querySelector(
        '[data-testid="dock-tabs"] [role=tablist] > *',
      );
      if (tab) lefts.push(tab.getBoundingClientRect().left);
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
    Object.assign(window, { __lefts: lefts });
  });
  await act();
  await page.waitForTimeout(1200);
  return page.evaluate(() =>
    (window as unknown as { __lefts: number[] }).__lefts.splice(0),
  );
}

for (const rest of ["remembered", "widest"] as const) {
  test(`the dock's tabs follow the divider as the pane hides and returns (${rest})`, async ({
    page,
  }, testInfo) => {
    test.skip(testInfo.project.name !== "webkit-desktop", "one engine");
    await page.setViewportSize({ width: 1500, height: 900 });
    if (rest === "widest")
      await seedLocalStorage(page, { "gents-prototype-trace-width": "5000" });
    await gotoHarness(page, "mobile-performance");
    await page.locator('[data-testid="session-session-large"]').click();
    await page
      .getByTestId("transcript-panel")
      .getByText("stream-start")
      .last()
      .waitFor();
    await page.getByRole("button", { name: "More" }).first().click();
    await page.getByRole("menuitem", { name: "Started sessions" }).click();
    await page.waitForTimeout(1200);

    const handle = page.getByRole("separator", { name: "Resize panel" });
    const box = (await handle.boundingBox())!;
    const hiding = await tabEdgeDuring(page, async () => {
      await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
      await page.mouse.down();
      for (let i = 1; i <= 20; i += 1)
        await page.mouse.move(box.x - i * 40, box.y + box.height / 2);
      await page.mouse.up();
    });
    const right = hiding.slice(1).filter((x, i) => x - hiding[i]! > 0.5);
    expect(right).toEqual([]);

    const returning = await tabEdgeDuring(page, async () => {
      await page.getByTitle("Show the session").click();
    });
    const left = returning.slice(1).filter((x, i) => returning[i]! - x > 0.5);
    expect(left).toEqual([]);
  });
}

test("the dock's + opens a panel that isn't open yet, and stays in view when tabs overflow", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1500, height: 900 });
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  await page.getByTestId("transcript-panel").getByText("stream-start").last().waitFor();
  await page.getByRole("button", { name: "More" }).first().click();
  await page.getByRole("menuitem", { name: "Started sessions" }).click();

  const tabs = page.getByRole("tablist", { name: "Panels" });
  const add = page.getByRole("button", { name: "Open a panel" });
  await add.click();
  /* only the panels not open yet */
  await expect(page.getByRole("menuitem")).toHaveText(["Background", "Changes"]);
  await page.getByRole("menuitem", { name: "Background" }).click();
  await expect(tabs.getByRole("tab", { name: "Background" })).toHaveAttribute(
    "aria-selected",
    "true",
  );

  /* squeezed, the tabs scroll and fade under the + rather than push it out */
  await page
    .getByTestId("dock-tabs")
    .evaluate((el) => ((el as HTMLElement).style.maxWidth = "190px"));
  await expect(tabs).toHaveAttribute("data-cut", "true");
  await expect(add).toBeInViewport();
  const addBox = (await add.boundingBox())!;
  const stripBox = (await tabs.boundingBox())!;
  expect(stripBox.x + stripBox.width).toBeLessThanOrEqual(addBox.x + 1);
  await page
    .getByTestId("dock-tabs")
    .evaluate((el) => ((el as HTMLElement).style.maxWidth = ""));

  /* with every panel open there is nothing to add */
  await add.click();
  await page.getByRole("menuitem", { name: "Changes" }).click();
  await expect(add).toHaveCount(0);
});

test("renaming keeps field focus, then the menu resumes native focus return", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1500, height: 900 });
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  await page.getByTestId("transcript-panel").getByText("stream-start").last().waitFor();
  const more = page.getByRole("button", { name: "More", exact: true }).first();
  await more.click();
  await page.getByRole("menuitem", { name: "Rename", exact: true }).click();
  const name = page.getByRole("textbox", { name: /^Rename / }).first();
  await expect(name).toBeFocused();
  await expect(page.getByRole("menu")).toHaveCount(0);
  await expect(name).toBeFocused();
  await page.keyboard.press("Escape");
  await more.click();
  await expect(page.getByRole("menu")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("menu")).toHaveCount(0);
  await expect(more).toBeFocused();

  // A close animation must not steal focus deliberately moved to the composer.
  await page.addStyleTag({
    content:
      '[data-slot="dropdown-menu-content"][data-closed] { animation-duration: 300ms !important; }',
  });
  await more.click();
  await expect(page.getByRole("menu")).toBeFocused();
  await page.keyboard.press("Escape");
  const composer = page.getByRole("textbox", { name: "Message" });
  await composer.focus();
  await expect(page.locator('[data-slot="dropdown-menu-content"]')).toHaveCount(0);
  await expect(composer).toBeFocused();
});

test("the add-panel trigger stays fixed while its dock expands during a press", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1500, height: 900 });
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  await page.getByTestId("transcript-panel").getByText("stream-start").last().waitFor();
  await page.getByRole("button", { name: "More", exact: true }).first().click();
  await page.getByRole("menuitem", { name: "Started sessions" }).click();

  // Hold the outer edge fixed while the divider advances, as during dock opening.
  const layout = await page.addStyleTag({
    content: ".app-titlebar { grid-template-columns: 56px 1044px 400px !important; }",
  });
  const add = page.getByRole("button", { name: "Open a panel" });
  const before = (await add.boundingBox())!;
  await page.mouse.move(before.x + before.width / 2, before.y + before.height / 2);
  await page.mouse.down();
  await layout.evaluate((el) => {
    el.textContent =
      ".app-titlebar { grid-template-columns: 56px 744px 700px !important; }";
  });
  const after = (await add.boundingBox())!;
  expect(Math.abs(after.x - before.x)).toBeLessThanOrEqual(1);
  await page.mouse.up();
  await page.getByRole("menuitem", { name: "Background", exact: true }).click();
  await expect(
    page.getByRole("tab", { name: "Background", exact: true }),
  ).toHaveAttribute("aria-selected", "true");
});

test("the dock's expand button fills the window with the panel and gives the session back", async ({
  page,
}, testInfo) => {
  test.skip(testInfo.project.name !== "webkit-desktop", "one engine");
  await page.setViewportSize({ width: 1500, height: 900 });
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  await page.getByTestId("transcript-panel").getByText("stream-start").last().waitFor();
  await page.getByRole("button", { name: "More" }).first().click();
  await page.getByRole("menuitem", { name: "Started sessions" }).click();

  const dock = page.getByTestId("dock");
  const narrow = (await dock.boundingBox())!.width;
  await page.getByRole("button", { name: "Expand panel" }).click();
  await expect(page.getByTitle("Show the session")).toBeVisible();
  await expect
    .poll(async () => (await dock.boundingBox())!.width)
    .toBeGreaterThan(1500 - 56 - 40);

  await page.getByRole("button", { name: "Restore panel" }).click();
  await expect(page.getByRole("button", { name: "Expand panel" })).toBeVisible();
  await expect
    .poll(async () => Math.round((await dock.boundingBox())!.width))
    .toBe(Math.round(narrow));
});
