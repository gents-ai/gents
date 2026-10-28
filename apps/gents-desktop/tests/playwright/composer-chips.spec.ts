import { expect, gotoHarness, openChat, test, type Page } from "./desktopTest";

const PASTE = Array.from(
  { length: 8 },
  (_, i) => `line ${i + 1}: some pasted text that runs long enough to wrap`,
).join("\n");

const field = (page: Page) => page.getByRole("textbox", { name: "Message" });
const chips = (page: Page) => page.locator('[data-slot="paste-chip"]');
const card = (page: Page) => page.locator('[data-slot="paste-chip-preview"]');
/* the field's text as shown, without the zero-width guard before a chip
   that starts a line */
const shownText = (page: Page) =>
  field(page).evaluate((el) => (el.textContent ?? "").replaceAll("\u200B", ""));

async function paste(page: Page, text = PASTE) {
  await field(page).evaluate((el, value) => {
    const data = new DataTransfer();
    data.setData("text/plain", value);
    el.dispatchEvent(
      new ClipboardEvent("paste", {
        clipboardData: data,
        bubbles: true,
        cancelable: true,
      }),
    );
  }, text);
}

test.describe("paste chips in the composer", () => {
  /* wide enough that no line wraps: a soft wrap spends an arrow press */
  test.use({ viewport: { width: 1280, height: 900 } });

  test("a long paste becomes a chip the caret steps around", async ({ page }) => {
    await gotoHarness(page);
    await openChat(page);
    await field(page).click();
    await page.keyboard.type("see ");
    await paste(page);
    await expect(chips(page)).toHaveText("Pasted 8 lines");
    await page.keyboard.type(" after");
    for (let i = 0; i < 7; i++) await page.keyboard.press("ArrowLeft");
    await page.keyboard.type("X");
    await page.keyboard.press("ArrowRight");
    await page.keyboard.type("Z");
    await expect(field(page)).toContainText("see XPasted 8 linesZ after");
    await expect(chips(page)).toHaveCount(1);
  });

  test("a paste of fewer than five lines stays text", async ({ page }) => {
    await gotoHarness(page);
    await openChat(page);
    await field(page).click();
    await paste(page, "one\ntwo\nthree\nfour");
    await expect(chips(page)).toHaveCount(0);
    await expect(field(page)).toContainText("four");
  });

  test("a chip at the start of a line takes text on either side", async ({ page }) => {
    await gotoHarness(page);
    await openChat(page);
    await field(page).click();
    await paste(page);
    await page.keyboard.type(" after");
    await page.keyboard.press(
      process.platform === "darwin" ? "Meta+ArrowLeft" : "Home",
    );
    await page.keyboard.type("before ");
    await expect(field(page)).toContainText("before Pasted 8 lines after");
    await expect(chips(page)).toHaveCount(1);
  });

  test("hovering or clicking a chip shows its paste, and Expand text puts it back", async ({
    page,
  }) => {
    await gotoHarness(page);
    await openChat(page);
    await field(page).click();
    await paste(page);
    await chips(page).hover();
    await expect(card(page)).toBeVisible();
    await expect(card(page)).toContainText("Pasted 8 lines");
    await expect(card(page)).toContainText("line 8: some pasted text");
    await page.mouse.move(0, 0);
    await expect(card(page)).toBeHidden();
    await chips(page).click();
    await card(page).getByRole("button", { name: "Expand text" }).click();
    await expect(chips(page)).toHaveCount(0);
    await expect(field(page)).toContainText("line 1: some pasted text");
  });

  test("Backspace takes a chip whole, and undo brings it back", async ({ page }) => {
    await gotoHarness(page);
    await openChat(page);
    await field(page).click();
    await page.keyboard.type("see ");
    await paste(page);
    await expect(chips(page)).toHaveCount(1);
    await page.keyboard.press("Backspace");
    await expect(chips(page)).toHaveCount(0);
    await expect(field(page)).toHaveText("see ");
    await page.keyboard.press("ControlOrMeta+z");
    await expect(chips(page)).toHaveCount(1);
  });

  test("a click left of a chip that starts a line puts the caret before it", async ({
    page,
  }) => {
    await gotoHarness(page);
    await openChat(page);
    await field(page).click();
    await paste(page);
    await page.keyboard.type(" after");
    const box = (await chips(page).boundingBox())!;
    await page.mouse.click(box.x - 3, box.y + box.height / 2);
    await page.keyboard.type("before ");
    await expect(field(page)).toContainText("before Pasted 8 lines after");
    await expect(chips(page)).toHaveCount(1);
  });

  test("Shift+← takes a chip into the selection as one character", async ({ page }) => {
    await gotoHarness(page);
    await openChat(page);
    await field(page).click();
    await page.keyboard.type("see ");
    await paste(page);
    await page.keyboard.type(" x");
    for (let i = 0; i < 3; i++) await page.keyboard.press("Shift+ArrowLeft");
    await page.keyboard.type("Y");
    await expect(chips(page)).toHaveCount(0);
    await expect(field(page)).toHaveText("see Y");
  });

  test("Delete just before a chip takes it whole", async ({ page }) => {
    await gotoHarness(page);
    await openChat(page);
    await field(page).click();
    await page.keyboard.type("see ");
    await paste(page);
    await page.keyboard.press("ArrowLeft");
    await page.keyboard.press("Delete");
    await expect(chips(page)).toHaveCount(0);
    await expect(field(page)).toHaveText("see ");
  });

  test("Backspace at the start of the next line joins it to the chip's line", async ({
    page,
  }) => {
    await gotoHarness(page);
    await openChat(page);
    await field(page).click();
    await paste(page);
    await page.keyboard.press("Shift+Enter");
    await page.keyboard.type("next");
    for (let i = 0; i < 4; i++) await page.keyboard.press("ArrowLeft");
    await page.keyboard.press("Backspace");
    await expect(chips(page)).toHaveCount(1);
    await expect.poll(() => shownText(page)).toBe("Pasted 8 linesnext");
  });

  test("typing over a selection that holds a chip replaces it", async ({ page }) => {
    await gotoHarness(page);
    await openChat(page);
    await field(page).click();
    await page.keyboard.type("see ");
    await paste(page);
    await page.keyboard.type(" after");
    await page.keyboard.press("ControlOrMeta+a");
    await page.keyboard.type("fresh");
    await expect(chips(page)).toHaveCount(0);
    await expect(field(page)).toHaveText("fresh");
  });

  test("a draft keeps its chip when the person leaves and comes back", async ({
    page,
  }) => {
    await gotoHarness(page);
    await page.getByTestId("session-session-intro").click();
    await field(page).click();
    await page.keyboard.type("see ");
    await paste(page);
    await expect(chips(page)).toHaveCount(1);
    await page.getByRole("link", { name: "New session" }).first().click();
    await expect(chips(page)).toHaveCount(0);
    await page.getByRole("button", { name: "Back" }).click();
    await expect(chips(page)).toHaveText("Pasted 8 lines");
    await expect(field(page)).toContainText("see Pasted 8 lines");
  });
});

test("a skill picked from the slash menu becomes a command chip", async ({ page }) => {
  await gotoHarness(page);
  await openChat(page);
  await field(page).click();
  await page.keyboard.type("/host");
  await page.keyboard.press("Enter");
  await expect(page.locator('[data-slot="command-chip"]')).toHaveText(
    "/host-diagnostics",
  );
});

test("the slash menu opens at the composer and offers the last pick first", async ({
  page,
}) => {
  await gotoHarness(page);
  await openChat(page);
  await field(page).click();
  await page.keyboard.type("/");
  const menu = page.locator('[data-slot="composer-menu"]');
  await expect(menu.getByRole("option").first()).toContainText("/goal");
  await page.keyboard.type("host");
  await page.keyboard.press("Enter");
  await page.keyboard.press("ControlOrMeta+a");
  await page.keyboard.press("Backspace");
  await page.keyboard.type("/");
  await expect(menu.getByRole("option").first()).toContainText("/host-diagnostics");
  const box = await menu.boundingBox();
  const frame = await page.locator('[data-slot="composer"]').last().boundingBox();
  expect(Math.round(box!.width)).toBe(Math.round(frame!.width));
  expect(box!.y + box!.height).toBeLessThanOrEqual(frame!.y);
});

test("a skill picked mid-sentence becomes a chip and is named on an opening line when sent", async ({
  page,
}) => {
  await gotoHarness(page);
  await openChat(page);
  await field(page).click();
  await page.keyboard.type("please /");
  const menu = page.locator('[data-slot="composer-menu"]');
  /* a command acts at once, so only the opening lines offer it */
  await expect(menu.getByRole("option").first()).toContainText("/host-diagnostics");
  await expect(menu.getByRole("option", { name: /\/goal/ })).toHaveCount(0);
  await page.keyboard.type("host");
  await page.keyboard.press("Enter");
  await expect(page.locator('[data-slot="command-chip"]')).toHaveText(
    "/host-diagnostics",
  );
  await page.keyboard.type("now");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("transcript-panel")).toContainText(
    /\/skill host-diagnostics\s+please \/host-diagnostics now/,
  );
  /* the draft clears though what went out has an added opening line */
  await expect(field(page)).toHaveText("");
});

test("a session picked from the @ menu becomes a chip, sends a link and shows as a chip that opens it", async ({
  page,
}) => {
  await gotoHarness(page);
  await openChat(page);
  await field(page).click();
  await page.keyboard.type("see @intro");
  const menu = page.locator('[data-slot="composer-menu"]');
  await expect(menu.getByRole("option").first()).toContainText(
    "introduction-and-greetings",
  );
  await page.keyboard.press("Enter");
  const picked = page.locator('[data-slot="session-chip"]');
  await expect(picked).toHaveText("introduction-and-greetings");
  /* its kind's icon says what it is, and hovering it opens the card */
  await expect(picked.locator("svg")).toHaveCount(1);
  await picked.locator("svg").hover();
  await expect(page.locator('[data-slot="session-chip-preview"]')).toContainText(
    "introduction-and-greetings",
  );
  await field(page).click();
  await page.keyboard.type("first");
  await page.keyboard.press("Enter");
  const transcript = page.getByTestId("transcript-panel");
  const sent = transcript.locator('[data-slot="session-chip"]');
  await expect(sent).toHaveText("introduction-and-greetings");
  await expect(transcript).toContainText("see introduction-and-greetings first");
  await sent.click();
  await expect(page.getByTestId("session-screen")).toContainText(
    "introduction-and-greetings",
  );
  await expect(transcript).not.toContainText("see introduction-and-greetings first");
});

test("an @ inside a word opens no menu", async ({ page }) => {
  await gotoHarness(page);
  await openChat(page);
  await field(page).click();
  await page.keyboard.type("name@intro");
  await expect(page.locator('[data-slot="composer-menu"]')).toHaveCount(0);
});

test.describe("on a touch screen", () => {
  test.use({ hasTouch: true, isMobile: true, viewport: { width: 390, height: 844 } });

  test("a tap opens the chip's card, which stays open for its buttons", async ({
    page,
  }, info) => {
    test.skip(info.project.name.startsWith("webkit"), "touch emulation is Chromium's");
    await gotoHarness(page);
    await openChat(page);
    await field(page).tap();
    await paste(page);
    await chips(page).tap();
    await expect(card(page)).toBeVisible();
    await page.waitForTimeout(600);
    await expect(card(page)).toBeVisible();
    await card(page).getByRole("button", { name: "Remove" }).tap();
    await expect(chips(page)).toHaveCount(0);
  });
});
