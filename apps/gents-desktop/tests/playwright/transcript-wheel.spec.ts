import { expect, gotoHarness, test } from "./desktopTest";

test("wheeling up past an open activity box keeps the transcript moving @own-viewport", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1280, height: 900 });
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  const panel = page.getByTestId("transcript-panel");
  await panel.getByText("stream-start", { exact: false }).last().waitFor();
  await page.evaluate(() => window.__GENTS_MOBILE_PERFORMANCE__!.toolBatch(24));
  const group = panel.getByRole("button", { name: /24 commands/ });
  await group.scrollIntoViewIfNeeded();
  await group.click();
  await expect(group).toHaveAttribute("aria-expanded", "true");

  const scroller = panel.locator(
    "xpath=ancestor::*[@data-slot='scroll-area-viewport'][1]",
  );
  const box = group.locator("xpath=following-sibling::*[1]");
  await box.evaluate((el) => {
    const viewport = el.querySelector<HTMLElement>("[data-slot=scroll-area-viewport]");
    if (viewport) viewport.scrollTop = 0;
  });
  const rect = (await box.boundingBox())!;
  await page.mouse.move(rect.x + rect.width / 2, rect.y + 20);

  const before = await scroller.evaluate((el) => el.scrollTop);
  for (let notch = 0; notch < 6; notch += 1) {
    await page.mouse.wheel(0, -100);
    await page.waitForTimeout(50);
  }
  await expect
    .poll(() => scroller.evaluate((el) => el.scrollTop))
    .toBeLessThan(before - 200);
});

test("a group with a failed command says so and opens its first failure @own-viewport", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1280, height: 900 });
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  const panel = page.getByTestId("transcript-panel");
  await panel.getByText("stream-start", { exact: false }).last().waitFor();
  await page.evaluate(() => window.__GENTS_MOBILE_PERFORMANCE__!.toolBatch(6, 2));
  const group = panel.getByRole("button", { name: /6 commands/ });
  await group.scrollIntoViewIfNeeded();
  await expect(group).toHaveText(/Ran 6 commands, 2 failed/);
  await expect(group).toHaveAttribute("aria-expanded", "true");
  await expect(group).toHaveAttribute("data-failed", "true");
  await expect(panel.locator('[data-step-key="batch-exec-4"]')).toHaveAttribute(
    "data-open",
  );
  const passed = panel.locator('[data-step-key="batch-exec-0"]');
  await expect(passed).toBeVisible();
  await expect(passed).not.toHaveAttribute("data-open");
});

test("a command's long output scrolls inside its own box @own-viewport", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1280, height: 900 });
  await gotoHarness(page, "mobile-performance");
  await page.locator('[data-testid="session-session-large"]').click();
  const panel = page.getByTestId("transcript-panel");
  await panel.getByText("stream-start", { exact: false }).last().waitFor();
  await page.evaluate(() => window.__GENTS_MOBILE_PERFORMANCE__!.toolBatch(3, 0, 60));
  const group = panel.getByRole("button", { name: /3 commands/ });
  await group.scrollIntoViewIfNeeded();
  if ((await group.getAttribute("aria-expanded")) !== "true") await group.click();
  await panel.getByTitle("$ cargo test -p gents step_0").click();
  const output = panel.getByText("output line 59");
  await output.waitFor();
  const box = output.locator("xpath=ancestor::*[@data-slot='scroll-area-viewport'][1]");
  await expect
    .poll(() => box.evaluate((el) => el.scrollHeight > el.clientHeight + 100))
    .toBe(true);
  await box.evaluate((el) => {
    window.addEventListener(
      "wheel",
      (event) => {
        if (el.contains(event.target as Node)) {
          el.dataset.wheelLanded = "inside";
          return;
        }
        const target = event.target as HTMLElement;
        const { top, bottom, left, right } = el.getBoundingClientRect();
        el.dataset.wheelLanded = JSON.stringify({
          x: event.clientX,
          y: event.clientY,
          box: { top, bottom, left, right },
          target: `${target.tagName} ${target.dataset?.slot ?? ""}`.trim(),
        });
      },
      /* not passive, so the target is read before the wheel scrolls anything */
      { capture: true, once: true, passive: false },
    );
  });
  /* the step's panel opens after the click and clips the box until it does,
     so the point is taken once the box is under it for two frames. Nothing
     here may scroll: a scroll the reader did not make is put back by the
     transcript, which moves the box from under the point */
  let measured = { x: 0, y: 0 };
  await expect
    .poll(
      async () =>
        (measured = await box.evaluate(async (el) => {
          const read = async () => {
            await new Promise((resolve) => requestAnimationFrame(resolve));
            const { top, left, width } = el.getBoundingClientRect();
            const x = left + width / 2;
            const y = top + 20;
            const hit = document.elementFromPoint(x, y);
            return {
              x,
              y,
              top,
              left,
              hit: hit
                ? `${hit.tagName} ${(hit as HTMLElement).dataset?.slot ?? ""}`.trim()
                : "none",
              inside: !!hit && el.contains(hit),
            };
          };
          const first = await read();
          const second = await read();
          return {
            ...second,
            held:
              first.inside &&
              second.inside &&
              first.top === second.top &&
              first.left === second.left,
          };
        })),
      { timeout: 0 },
    )
    .toEqual(expect.objectContaining({ held: true }));
  await page.mouse.move(measured.x, measured.y);
  await page.mouse.wheel(0, 200);
  await expect(box).toHaveAttribute("data-wheel-landed", "inside");
  await expect.poll(() => box.evaluate((el) => el.scrollTop)).toBeGreaterThan(100);
});
