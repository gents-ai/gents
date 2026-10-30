import type { Locator, Page } from "@playwright/test";
import { expect, gotoHarness, test } from "./desktopTest";

async function keyboard(page: Page, height = 300, top = 50) {
  await page.evaluate(
    ({ height, top }) => {
      Object.defineProperties(window.visualViewport, {
        height: { configurable: true, value: height },
        offsetTop: { configurable: true, value: top },
      });
      window.visualViewport!.dispatchEvent(new Event("resize"));
    },
    { height, top },
  );
}

async function swipe(target: Locator) {
  await target.evaluate((el) => {
    const at = (x: number) => ({ clientX: x, clientY: 200 });
    el.dispatchEvent(
      Object.assign(new Event("touchstart", { bubbles: true }), {
        touches: [at(10)],
      }),
    );
    el.dispatchEvent(
      Object.assign(new Event("touchend", { bubbles: true }), {
        changedTouches: [at(140)],
      }),
    );
  });
}

test.beforeEach(async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/tests/ui-harness/mobile.html");
});

test("editor sheets fit above the keyboard", async ({ page }) => {
  await page.getByRole("button", { name: "Open editor", exact: true }).click();
  await keyboard(page);
  const sheet = page.getByRole("dialog", { name: "Mobile editor" });
  await expect
    .poll(() => sheet.evaluate((el) => Math.round(el.getBoundingClientRect().top)))
    .toBe(50);
  await expect
    .poll(() => sheet.evaluate((el) => Math.round(el.getBoundingClientRect().bottom)))
    .toBe(350);
  await keyboard(page, 844, 0);
  await expect
    .poll(() => sheet.evaluate((el) => Math.round(el.getBoundingClientRect().bottom)))
    .toBe(844);
});

test("dialogs fit inside the visible viewport", async ({ page }) => {
  await page.getByRole("button", { name: "Open dialog", exact: true }).click();
  await keyboard(page);
  const dialog = page.getByRole("dialog", { name: "Mobile dialog" });
  await expect
    .poll(() => dialog.evaluate((el) => el.getBoundingClientRect().bottom))
    .toBeLessThanOrEqual(350);
  await expect
    .poll(() => dialog.evaluate((el) => el.getBoundingClientRect().top))
    .toBeGreaterThanOrEqual(50);
  const save = page.getByRole("button", { name: "Save dialog" });
  /* the person's own scroll takes over from revealing the focused field;
     a script scroll does not, and the keyboard's settled reveal scrolls the
     field back over it */
  await dialog.hover();
  await page.mouse.wheel(0, 600);
  await expect
    .poll(() => save.evaluate((el) => el.getBoundingClientRect().bottom))
    .toBeLessThanOrEqual(350);
  await expect(save).toBeInViewport();
});

test("focusing a field reveals it in the editor's real scroll container", async ({
  page,
}) => {
  await page.getByRole("button", { name: "Open editor", exact: true }).click();
  await keyboard(page);
  const field = page.getByRole("textbox", { name: "Last field" });
  await field.evaluate((el) => {
    el.closest(".overflow-y-auto")!.scrollTop = 0;
    el.focus({ preventScroll: true });
  });
  await expect
    .poll(() => field.evaluate((el) => el.getBoundingClientRect().bottom))
    .toBeLessThanOrEqual(334);
});

test("an edge swipe inside a dialog does not navigate the underlying page", async ({
  page,
}) => {
  await page.getByRole("button", { name: "Open editor", exact: true }).click();
  await swipe(page.getByRole("dialog", { name: "Mobile editor" }));
  await expect(page.getByTestId("back-count")).toHaveText("0");
  await page.getByRole("button", { name: "Close", exact: true }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await swipe(page.getByTestId("page-swipe"));
  await expect(page.getByTestId("back-count")).toHaveText("1");
});

test("horizontal scrolling and text selection do not navigate back", async ({
  page,
}) => {
  const horizontal = page.getByTestId("horizontal");
  await horizontal.evaluate((el) => {
    el.scrollLeft = 100;
  });
  await swipe(horizontal);
  await swipe(page.getByRole("textbox", { name: "Draft", exact: true }));
  await swipe(page.getByRole("textbox", { name: "Rich draft" }));
  await expect(page.getByTestId("back-count")).toHaveText("0");
  await horizontal.evaluate((el) => {
    el.scrollLeft = 0;
  });
  await swipe(horizontal);
  await expect(page.getByTestId("back-count")).toHaveText("1");
  await swipe(page.getByTestId("page-swipe"));
  await expect(page.getByTestId("back-count")).toHaveText("2");
});

test("bottom sheets stay above the keyboard", async ({ page }) => {
  await page.getByRole("button", { name: "Open bottom sheet", exact: true }).click();
  await keyboard(page);
  const sheet = page.getByRole("dialog", { name: "Bottom sheet" });
  await expect
    .poll(() => sheet.evaluate((el) => Math.round(el.getBoundingClientRect().bottom)))
    .toBe(350);
});

test("search stays reachable with the keyboard open", async ({ page }) => {
  await gotoHarness(page);
  await page.getByRole("button", { name: "Menu", exact: true }).click();
  await page.getByRole("button", { name: "Search", exact: true }).click();
  const search = page.getByTestId("search-palette");
  await expect(search).toBeVisible();
  await expect
    .poll(() => search.evaluate((el) => el.getBoundingClientRect().top))
    .toBeCloseTo(844 * 0.12, 0);
  await keyboard(page);
  await expect
    .poll(() => search.evaluate((el) => el.getBoundingClientRect().top))
    .toBeCloseTo(86, 0);
  await expect
    .poll(() => search.evaluate((el) => el.getBoundingClientRect().top))
    .toBeGreaterThanOrEqual(50);
  await expect
    .poll(() => search.evaluate((el) => el.getBoundingClientRect().bottom))
    .toBeLessThanOrEqual(350);
});

test("reading away from a focused field survives viewport scroll notifications", async ({
  page,
}) => {
  await page.getByRole("button", { name: "Open editor", exact: true }).click();
  await keyboard(page);
  const field = page.getByRole("textbox", { name: "Last field" });
  await field.focus();
  await expect
    .poll(() => field.evaluate((el) => el.getBoundingClientRect().bottom))
    .toBeLessThanOrEqual(334);
  await field.evaluate((el) => {
    el.dispatchEvent(new Event("touchmove", { bubbles: true }));
    el.closest(".overflow-y-auto")!.scrollTop = 0;
    window.visualViewport!.dispatchEvent(new Event("scroll"));
  });
  await page.waitForTimeout(400);
  expect(await field.evaluate((el) => el.closest(".overflow-y-auto")!.scrollTop)).toBe(
    0,
  );
});
