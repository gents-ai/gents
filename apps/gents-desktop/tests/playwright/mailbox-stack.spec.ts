import { expect, gotoHarness, test, type Page } from "./desktopTest";

async function openMailbox(page: Page) {
  const link = page.getByRole("link", { name: /^Mailbox(?:\s+\d+)?$/ });
  if (!(await link.first().isVisible()))
    await page.getByRole("button", { name: "Menu", exact: true }).click();
  await link.last().click();
  await expect(page.getByTestId("mailbox-screen")).toBeVisible();
}

const rowButton = (page: Page, title: string) =>
  page
    .getByTestId("mailbox-item")
    .filter({ hasText: title })
    .getByRole("button")
    .first();

test("works through the mailbox in its dialog, and keeps the place across a session", async ({
  page,
}) => {
  await gotoHarness(page, "mailbox-many");
  await openMailbox(page);
  const dialog = page.getByTestId("mailbox-dialog");

  await rowButton(page, "Button colour").click();
  await expect(dialog).toContainText("1 of 5");
  await expect(dialog).toContainText("Which colour should the button be?");
  await page.keyboard.press("ArrowRight");
  await expect(dialog).toContainText("Where should this run?");

  await dialog.getByText("Open session", { exact: true }).click();
  await expect(page.getByTestId("session-screen")).toBeVisible();
  await expect(dialog).toHaveCount(0);
  await page.getByRole("button", { name: "Back" }).first().click();
  await expect(page.getByTestId("mailbox-screen")).toBeVisible();
  await expect(dialog).toContainText("Where should this run?");

  await page.keyboard.press("Escape");
  await expect(dialog).toHaveCount(0);
});

test("opens the mailbox's dialog over another page with its shortcut", async ({
  page,
}) => {
  await gotoHarness(page, "mailbox-many");
  await expect(page.getByTestId("sessions-screen")).toBeVisible();
  const dialog = page.getByTestId("mailbox-dialog");
  await page.keyboard.press("ControlOrMeta+Shift+KeyM");
  await expect(dialog).toContainText("Which colour should the button be?");
  await expect(page.getByTestId("sessions-screen")).toBeVisible();
  await page.keyboard.press("ControlOrMeta+Shift+KeyM");
  await expect(dialog).toHaveCount(0);
});

test.describe("mobile mailbox", () => {
  test.use({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true });

  async function keyboard(page: Page) {
    await page.evaluate(() => {
      Object.defineProperties(window.visualViewport, {
        height: { configurable: true, value: 300 },
        offsetTop: { configurable: true, value: 50 },
      });
      window.visualViewport!.dispatchEvent(new Event("resize"));
    });
  }

  async function visibleAboveKeyboard(locator: import("@playwright/test").Locator) {
    await expect
      .poll(() => locator.evaluate((el) => el.getBoundingClientRect().top))
      .toBeGreaterThanOrEqual(50);
    await expect
      .poll(() => locator.evaluate((el) => el.getBoundingClientRect().bottom))
      .toBeLessThanOrEqual(350);
  }

  test("every mailbox tab is reachable without overflowing the page", async ({
    page,
  }) => {
    await gotoHarness(page, "mailbox-many");
    await openMailbox(page);
    const strip = page.getByTestId("mailbox-tabs-scroll");
    expect(await strip.evaluate((el) => el.scrollWidth)).toBeGreaterThan(
      await strip.evaluate((el) => el.clientWidth),
    );
    await strip.evaluate((el) => {
      el.scrollLeft = el.scrollWidth;
    });
    const done = page.getByRole("tab", { name: /^Done/ });
    await expect(done).toBeInViewport();
    await done.tap();
    await expect(done).toHaveAttribute("aria-selected", "true");
    const pageWidth = await page.evaluate(() => document.documentElement.scrollWidth);
    expect(pageWidth).toBeLessThanOrEqual(390);
  });

  test("the question body scrolls while close and actions stay above the keyboard", async ({
    page,
  }) => {
    await gotoHarness(page, "mailbox-many");
    await openMailbox(page);
    await rowButton(page, "Button colour").tap();
    await keyboard(page);
    const dialog = page.getByTestId("mailbox-dialog");
    const close = dialog.getByRole("button", { name: "Close mailbox" });
    await visibleAboveKeyboard(close);
    await visibleAboveKeyboard(dialog.getByRole("button", { name: "Open session" }));
    const body = dialog.locator('[data-testid="question-panel"] > div.min-h-0');
    expect(await body.evaluate((el) => el.scrollHeight)).toBeGreaterThan(
      await body.evaluate((el) => el.clientHeight),
    );
    await body.evaluate((el) => {
      el.dispatchEvent(new Event("touchmove", { bubbles: true }));
      el.scrollTop = el.scrollHeight;
    });
    await visibleAboveKeyboard(close);
    await close.tap();
    await expect(dialog).toHaveCount(0);
    await expect(rowButton(page, "Button colour")).toBeVisible();
  });

  test("a detail card can close without dismissing its item with the keyboard open", async ({
    page,
  }) => {
    await gotoHarness(page, "mailbox-overflow");
    await openMailbox(page);
    await page.getByTestId("mailbox-item").getByRole("button").first().tap();
    await keyboard(page);
    const dialog = page.getByTestId("mailbox-dialog");
    const close = dialog.getByRole("button", { name: "Close mailbox" });
    await visibleAboveKeyboard(close);
    await visibleAboveKeyboard(
      dialog.getByRole("button", { name: "Dismiss", exact: true }),
    );
    await close.tap();
    await expect(dialog).toHaveCount(0);
    await expect(page.getByTestId("mailbox-item")).toHaveCount(1);
  });

  test("an inner control's consumed arrow does not advance the mailbox", async ({
    page,
  }) => {
    await gotoHarness(page, "mailbox-many");
    await openMailbox(page);
    await rowButton(page, "Button colour").tap();
    const dialog = page.getByTestId("mailbox-dialog");
    await dialog.getByRole("radio", { name: /Blue/ }).evaluate((el) => {
      el.addEventListener("keydown", (e) => e.preventDefault(), { once: true });
      el.dispatchEvent(
        new KeyboardEvent("keydown", {
          key: "ArrowRight",
          bubbles: true,
          cancelable: true,
        }),
      );
    });
    await expect(dialog).toContainText("Which colour should the button be?");
  });
});
