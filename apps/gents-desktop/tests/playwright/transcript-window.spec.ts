import type { Page } from "@playwright/test";
import { expect, gotoHarness, test } from "./desktopTest";

/* The transcript draws only the rows near the view. Wherever the list
   shows in the view, a drawn row must cover it: a gap is a row the list
   has not drawn yet, seen by the reader as blank space. */
declare global {
  interface Window {
    __blankInView?: () => number;
    __painted?: number[];
  }
}

async function watchBlank(page: Page) {
  await page.evaluate(() => {
    const scroller = document
      .querySelector('[data-testid="transcript-panel"]')!
      .closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!;
    const list = scroller.querySelector<HTMLElement>("[data-transcript-list]")!;
    /* the widest stretch of the view, within the list, no drawn row covers */
    window.__blankInView = () => {
      const view = scroller.getBoundingClientRect();
      const box = list.getBoundingClientRect();
      const top = Math.max(view.top, box.top);
      const bottom = Math.min(view.bottom, box.bottom);
      if (bottom <= top) return 0;
      const spans = [...list.querySelectorAll<HTMLElement>("[data-row-key]")]
        .map((row) => row.getBoundingClientRect())
        .filter((r) => r.bottom > top && r.top < bottom)
        .sort((a, b) => a.top - b.top);
      let reach = top;
      let widest = 0;
      for (const r of spans) {
        widest = Math.max(widest, r.top - reach);
        reach = Math.max(reach, r.bottom);
      }
      return Math.max(widest, bottom - reach);
    };
    /* sampled as each frame is about to paint: size observers deliver after
       every animation frame's work, so a sentinel resized every frame
       reports the layout the frame shows */
    const painted: number[] = [];
    window.__painted = painted;
    const sentinel = document.body.appendChild(document.createElement("div"));
    sentinel.style.cssText =
      "position:fixed;left:0;top:0;height:1px;pointer-events:none";
    new ResizeObserver(() => painted.push(window.__blankInView!())).observe(sentinel);
    let wide = false;
    const tick = () => {
      wide = !wide;
      sentinel.style.width = wide ? "2px" : "1px";
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  });
}

async function loadHistory(page: Page) {
  const panel = page.getByTestId("transcript-panel");
  await panel.hover();
  for (let i = 0; i < 40; i += 1) {
    await page.mouse.wheel(0, -1500);
    await page.waitForTimeout(150);
  }
  for (let i = 0; i < 40; i += 1) {
    await page.mouse.wheel(0, 1500);
    await page.waitForTimeout(100);
  }
  await page.waitForTimeout(500);
}

const loadedRows = (page: Page) =>
  page
    .locator("[data-transcript-list]")
    .evaluate((list) => Number(list.getAttribute("data-rows")));

test("a long transcript shows no blank row while the reader scrolls up", async ({
  page,
}, testInfo) => {
  test.skip(
    !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
    "one layout per engine",
  );
  test.slow();
  await page.setViewportSize({ width: 1500, height: 900 });
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  await page.getByTestId("transcript-panel").getByText("stream-start").last().waitFor();
  await watchBlank(page);

  await loadHistory(page);

  const blanks = await page.evaluate(() => window.__painted!);
  expect(await loadedRows(page)).toBeGreaterThan(200);
  expect(blanks.length).toBeGreaterThan(100);
  expect(blanks.filter((gap) => gap > 2)).toEqual([]);
});

test("a jump to a far part of a long transcript shows no blank row", async ({
  page,
}, testInfo) => {
  test.skip(
    !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
    "one layout per engine",
  );
  test.slow();
  await page.setViewportSize({ width: 1500, height: 900 });
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  await page.getByTestId("transcript-panel").getByText("stream-start").last().waitFor();
  await loadHistory(page);
  await watchBlank(page);
  expect(await loadedRows(page)).toBeGreaterThan(200);

  for (const fraction of [0.5, 0.1, 0.8, 0.05]) {
    const gaps = await page.evaluate(async (fraction) => {
      const scroller = document
        .querySelector('[data-testid="transcript-panel"]')!
        .closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!;
      const from = window.__painted!.length;
      scroller.scrollTop = fraction * (scroller.scrollHeight - scroller.clientHeight);
      for (let i = 0; i < 12; i += 1)
        await new Promise((r) => requestAnimationFrame(r));
      return window.__painted!.slice(from);
    }, fraction);
    expect(gaps.filter((gap) => gap > 2)).toEqual([]);
    await page.waitForTimeout(400);
  }
});
