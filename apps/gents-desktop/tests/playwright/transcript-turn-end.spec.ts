import type { Page } from "@playwright/test";
import { expect, gotoHarness, test } from "./desktopTest";

type Step = { live: "keep" | "drop"; saved: boolean; completed: boolean };

/* The ways the bridge can deliver the end of a turn: the saved reply and
   the completed request in one snapshot, or either one first. */
const ENDINGS: Record<string, Step[]> = {
  "the reply is saved as the turn completes": [
    { live: "drop", saved: true, completed: true },
  ],
  "the turn completes before the reply is saved": [
    { live: "drop", saved: false, completed: true },
    { live: "drop", saved: true, completed: true },
  ],
  "the reply is saved before the turn completes": [
    { live: "drop", saved: true, completed: false },
    { live: "drop", saved: true, completed: true },
  ],
};

const paragraph = (n: number) =>
  `\n\nParagraph ${n} of the reply carries enough words to wrap across the column, so the reply grows as tall as a long answer.\n\n\`\`\`\nblock ${n}: one\nblock ${n}: two\nblock ${n}: three\n\`\`\``;

async function streamLongReply(page: Page) {
  for (let n = 1; n <= 24; n += 1) {
    await page.evaluate(
      (text) => window.__GENTS_MOBILE_PERFORMANCE__!.streamText(text),
      paragraph(n),
    );
  }
  /* the reveal trails the stream */
  await expect(page.getByText("block 24: three")).toBeVisible({ timeout: 20_000 });
}

/* Puts the reader on the reply's twelfth paragraph, then notes where that
   paragraph sits on screen every frame from then on. */
async function readInsideReply(page: Page) {
  await page.evaluate(() => {
    const scroller = document
      .querySelector('[data-testid="transcript-panel"]')!
      .closest<HTMLElement>('[data-slot="scroll-area-viewport"]')!;
    const find = () =>
      Array.from(scroller.querySelectorAll("p")).find((p) =>
        p.textContent!.startsWith("Paragraph 12 of"),
      );
    /* as a reader does: their own wheel, then the scroll it makes */
    scroller.dispatchEvent(new WheelEvent("wheel", { deltaY: -1 }));
    scroller.scrollTop +=
      find()!.getBoundingClientRect().top - scroller.getBoundingClientRect().top - 200;
    const tops: (number | null)[] = [];
    const tick = () => {
      tops.push(find()?.getBoundingClientRect().top ?? null);
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
    Object.assign(window, { __readerTops: tops });
  });
}

test.describe("a turn ending under a reader", () => {
  test.beforeEach(async ({ page }, testInfo) => {
    test.skip(
      !["webkit-desktop", "chromium-desktop"].includes(testInfo.project.name),
      "one layout per engine",
    );
    await gotoHarness(page, "mobile-performance");
    await page.locator('[data-testid="session-session-large"]').click();
    await page
      .getByTestId("transcript-panel")
      .getByText("stream-start")
      .last()
      .waitFor();
    await streamLongReply(page);
  });

  for (const [ending, steps] of Object.entries(ENDINGS)) {
    test(`keeps the reader's paragraph still when ${ending}`, async ({ page }) => {
      await readInsideReply(page);
      await page.waitForTimeout(200);
      for (const step of steps) {
        await page.evaluate(
          (s) => window.__GENTS_MOBILE_PERFORMANCE__!.endReply(s),
          step,
        );
        await page.waitForTimeout(400);
      }
      await expect(page.getByTestId("live-assistant")).toHaveCount(0);
      const tops = await page.evaluate(
        () => (window as unknown as { __readerTops: (number | null)[] }).__readerTops,
      );
      /* every frame's offset from where the reader was, each told once */
      const moves = [
        ...new Set(
          tops.map((top) => (top === null ? "gone" : Math.round(top - tops[0]!))),
        ),
      ].filter((move) => move !== 0);
      expect(moves).toEqual([]);
    });
  }
});
