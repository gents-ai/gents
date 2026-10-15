import type { Page } from "@playwright/test";
import { expect, gotoHarness, test } from "./desktopTest";

/* every frame's width of the pane and of the dock's surface, while `act` runs */
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
        /* the surface while its column shows any of it */
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

/* The dock opens and closes without re-laying out what it sits beside: the
   pane and the dock's surface each take one width for the whole settle, so
   a long transcript is re-wrapped once, not at every frame of the motion. */
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
  /* the pane: its width before, then the one it settles at */
  expect(distinct(opening.map((w) => w.pane))).toHaveLength(2);
  /* the dock's surface: absent, then the width it opens to */
  expect(distinct(opening.map((w) => w.dock).filter((w) => w > 0))).toHaveLength(1);

  const closing = await widthsDuring(page, async () => {
    await page.getByRole("button", { name: "Hide panel" }).click();
  });
  expect(distinct(closing.map((w) => w.pane))).toHaveLength(2);
  expect(distinct(closing.map((w) => w.dock).filter((w) => w > 0))).toHaveLength(1);
});

/* A drag holds the pane at the width it had when the drag began: the
   transcript slides under the moving dock and is re-wrapped once, where
   the divider is let go. */
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
  /* held at its width through the drag */
  expect(distinct(during)).toHaveLength(1);
  /* then laid out once where the divider was let go */
  expect(distinct(widths.map((w) => w.pane))).toHaveLength(2);
});

/* WebKit restyles everything inside a size container at each width it
   takes, so a container over the transcript would restyle every row of a
   long session at each step of a drag or a dock settling. */
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

/* every frame's left edge of the dock's first tab, while `act` runs */
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

/* The dock's tabs ride the divider as the pane hides behind a full dock and
   comes back: they only ever move the way the divider does. The inset the
   tabs take once first in the bar, and the room after the session tab, grow
   and shrink with the divider's last stretch rather than stepping at either
   end, including for a dock that rests at its widest. */
for (const rest of ["remembered", "widest"] as const) {
  test(`the dock's tabs follow the divider as the pane hides and returns (${rest})`, async ({
    page,
  }, testInfo) => {
    test.skip(testInfo.project.name !== "webkit-desktop", "one engine");
    await page.setViewportSize({ width: 1500, height: 900 });
    if (rest === "widest")
      await page.addInitScript(() =>
        localStorage.setItem("gents-prototype-trace-width", "5000"),
      );
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
