import type { Locator, Page } from "@playwright/test";
import { expect, gotoHarness, test } from "./desktopTest";

type Cost = { rects: number; writes: number; rows: number; grew: boolean };

function scroller(page: Page) {
  return page.locator('[data-slot="scroll-area-viewport"][data-following]');
}

async function cost(viewport: Locator, act: "scroll" | "chunk"): Promise<Cost> {
  return viewport.evaluate(async (scroller, act) => {
    const rect = Element.prototype.getBoundingClientRect;
    const position = Object.getOwnPropertyDescriptor(Element.prototype, "scrollTop")!;
    let rects = 0;
    let writes = 0;
    const height = scroller.scrollHeight;
    Element.prototype.getBoundingClientRect = function (this: Element) {
      rects += 1;
      return rect.call(this);
    };
    Object.defineProperty(scroller, "scrollTop", {
      configurable: true,
      get: () => position.get!.call(scroller),
      set: (value: number) => {
        writes += 1;
        position.set!.call(scroller, value);
      },
    });
    try {
      if (act === "scroll") {
        scroller.dispatchEvent(new WheelEvent("wheel", { deltaY: -20 }));
        position.set!.call(scroller, position.get!.call(scroller) - 20);
        scroller.dispatchEvent(new Event("scroll"));
      } else {
        window.__GENTS_MOBILE_PERFORMANCE__!.streamText(
          "\n\nMore of the reply, long enough to wrap onto a second line in the column.",
        );
        const deadline = performance.now() + 3000;
        while (scroller.scrollHeight <= height && performance.now() < deadline)
          await new Promise((resolve) => requestAnimationFrame(resolve));
      }
      await new Promise((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(resolve)),
      );
      return {
        rects,
        writes,
        rows: scroller.querySelectorAll("[data-timeline-key]").length,
        grew: scroller.scrollHeight > height,
      };
    } finally {
      Element.prototype.getBoundingClientRect = rect;
      delete (scroller as Partial<HTMLElement>).scrollTop;
    }
  }, act);
}

async function loadEveryPage(page: Page) {
  await page.getByTestId("transcript-panel").hover();
  for (let i = 0; i < 40; i += 1) {
    await page.mouse.wheel(0, -1500);
    await page.waitForTimeout(150);
  }
  await page.waitForTimeout(600);
}

async function backToBottom(page: Page) {
  await page.getByRole("button", { name: "Back to bottom" }).click();
  await expect(scroller(page)).toHaveAttribute("data-following", "true");
  await page.waitForTimeout(300);
}

test.describe("what a transcript scroll costs", () => {
  test.beforeEach(async ({ page }, testInfo) => {
    test.skip(
      !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
      "one layout per engine",
    );
    test.slow();
    await page.setViewportSize({ width: 1280, height: 900 });
    await gotoHarness(page, "mobile-performance");
    await page.locator('[data-testid="session-session-large"]').click();
    await page
      .getByTestId("transcript-panel")
      .getByText("stream-start", { exact: false })
      .last()
      .waitFor();
  });

  test("measures a bounded number of boxes per event, however long the session", async ({
    page,
  }, testInfo) => {
    const viewport = scroller(page);

    await page.getByTestId("transcript-panel").hover();
    await page.mouse.wheel(0, -600);
    await expect(viewport).toHaveAttribute("data-following", "false");
    await page.waitForTimeout(800);
    const short = {
      scroll: await cost(viewport, "scroll"),
      chunk: await cost(viewport, "chunk"),
    };

    await loadEveryPage(page);
    const long = {
      scroll: await cost(viewport, "scroll"),
      chunk: await cost(viewport, "chunk"),
    };
    testInfo.annotations.push({
      type: "cost",
      description: JSON.stringify({ short, long }),
    });
    expect(long.scroll.rows).toBeGreaterThan(short.scroll.rows * 4);

    for (const at of [short, long]) {
      expect(at.chunk.grew).toBe(true);
      expect(at.scroll.rects).toBeLessThanOrEqual(16);
      expect(at.chunk.rects).toBeLessThanOrEqual(16);

      expect(at.scroll.writes).toBe(0);
    }

    expect(long.scroll.rects).toBeLessThanOrEqual(short.scroll.rects + 10);
    expect(long.chunk.rects).toBeLessThanOrEqual(short.chunk.rects + 4);
  });

  test("writes the position at most once per streamed chunk while following", async ({
    page,
  }, testInfo) => {
    const viewport = scroller(page);
    await expect(viewport).toHaveAttribute("data-following", "true");
    await page.waitForTimeout(500);
    const short = await cost(viewport, "chunk");
    await loadEveryPage(page);
    await backToBottom(page);
    const long = await cost(viewport, "chunk");
    testInfo.annotations.push({
      type: "cost",
      description: JSON.stringify({ short, long }),
    });
    for (const at of [short, long]) {
      expect(at.grew).toBe(true);

      expect(at.rects).toBeLessThanOrEqual(8);
      expect(at.writes).toBeLessThanOrEqual(2);
    }
    expect(long.rects).toBeLessThanOrEqual(short.rects + 2);
  });
});
