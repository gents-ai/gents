import type { Page } from "@playwright/test";
import { expect, gotoHarness, test } from "./desktopTest";

type Frame = { gap: number; offset: number | null };
declare global {
  interface Window {
    __frames: Frame[];
    __heldKey: string | null;
  }
}

/* each frame the list shows, as it is about to paint: size observers
   deliver after the frame's work, so a sentinel resized every frame
   reports what it shows. A frame records the widest space between
   neighbouring rows in the view, and where the held row sits */
async function watchFrames(page: Page) {
  await page.evaluate(() => {
    window.__frames = [];
    window.__heldKey = null;
    const sentinel = document.body.appendChild(document.createElement("div"));
    sentinel.style.cssText =
      "position:fixed;left:0;top:0;height:1px;pointer-events:none";
    new ResizeObserver(() => {
      const list = document.querySelector<HTMLElement>("[data-transcript-list]");
      const scroller = list?.closest<HTMLElement>('[data-slot="scroll-area-viewport"]');
      if (!list || !scroller || getComputedStyle(list).visibility === "hidden") return;
      const rows = [...list.querySelectorAll<HTMLElement>("[data-row-key]")];
      if (rows.length === 0) return;
      const view = scroller.getBoundingClientRect();
      let gap = 0;
      for (let i = 1; i < rows.length; i += 1) {
        if (Number(rows[i]!.dataset.index) !== Number(rows[i - 1]!.dataset.index) + 1)
          continue;
        const above = rows[i - 1]!.getBoundingClientRect();
        const below = rows[i]!.getBoundingClientRect();
        if (below.top > view.bottom || above.bottom < view.top) continue;
        gap = Math.max(gap, Math.abs(below.top - above.bottom));
      }
      const held = window.__heldKey
        ? list.querySelector<HTMLElement>(
            `[data-row-key="${CSS.escape(window.__heldKey)}"]`,
          )
        : null;
      const box = held?.getBoundingClientRect();
      window.__frames.push({
        gap,
        offset:
          box && box.bottom > view.top && box.top < view.bottom
            ? box.top - view.top
            : null,
      });
    }).observe(sentinel);
    let wide = false;
    const tick = () => {
      wide = !wide;
      sentinel.style.width = wide ? "2px" : "1px";
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  });
}

const framesShown = (page: Page) => page.evaluate(() => window.__frames.length);

/* Opening a session draws its rows at their measured sizes from the first
   frame shown. Opening it again returns to where the reader left it: when
   that place is far above the newest page, nothing shows until its rows
   arrive, so the foot never flashes before the place. */
test("a session opens and returns to the reader's place without a frame out of place", async ({
  page,
}, testInfo) => {
  test.skip(
    !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
    "one layout per engine",
  );
  test.slow();
  await page.setViewportSize({ width: 1500, height: 900 });
  await gotoHarness(page, "mobile-performance");
  await watchFrames(page);
  const session = page.locator('[data-testid="session-session-large"]');
  await session.click();
  const panel = page.getByTestId("transcript-panel");
  await panel.getByText("stream-start").last().waitFor();
  await expect.poll(() => framesShown(page)).toBeGreaterThan(10);
  const opened = await page.evaluate(() => window.__frames.map((frame) => frame.gap));
  expect(opened.filter((gap) => gap > 2)).toEqual([]);

  await panel.hover();
  for (let i = 0; i < 20; i += 1) {
    await page.mouse.wheel(0, -1500);
    await page.waitForTimeout(150);
  }
  await page.waitForTimeout(800);
  const saved = await page.evaluate(() => {
    const key = Object.keys(localStorage).find((k) =>
      k.startsWith("gents-transcript-place:"),
    );
    return key
      ? (JSON.parse(localStorage.getItem(key)!) as { key: string; offset: number })
      : null;
  });
  expect(saved).not.toBeNull();

  await page.getByRole("button", { name: "Back" }).first().click();
  await session.waitFor();
  /* the page around the place arrives a while after the session opens */
  await page.evaluate((key) => {
    window.__GENTS_MOBILE_PERFORMANCE__!.setOlderPageDelay(300);
    window.__frames.length = 0;
    window.__heldKey = key;
  }, saved!.key);
  await session.click();
  await expect.poll(() => framesShown(page)).toBeGreaterThan(20);

  const returned = await page.evaluate(() => window.__frames);
  /* every frame shown has the reader's row where they left it, and every
     row at its size */
  expect(returned.filter((frame) => frame.offset === null)).toEqual([]);
  for (const frame of returned)
    expect(Math.abs(frame.offset! - saved!.offset)).toBeLessThanOrEqual(2);
  expect(returned.map((frame) => frame.gap).filter((gap) => gap > 2)).toEqual([]);
});
