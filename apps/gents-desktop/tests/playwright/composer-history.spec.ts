import { expect, gotoHarness, openChat, test, type Page } from "./desktopTest";

const field = (page: Page) => page.getByRole("textbox", { name: "Message" });
const history = (page: Page) => page.getByTestId("composer-history");
const commandChips = (page: Page) => page.locator('[data-slot="command-chip"]');

async function sendTwo(page: Page) {
  await gotoHarness(page);
  await openChat(page);
  await field(page).click();
  await page.keyboard.type("first message");
  await page.keyboard.press("Enter");
  const transcript = page.getByTestId("transcript-panel");
  await expect(transcript).toContainText("first message");
  await expect(field(page)).toHaveText("");
  /* the new chat becomes its session's page, and the composer its own */
  await field(page).click();
  await page.keyboard.type("please /host");
  await page.keyboard.press("Enter");
  await expect(commandChips(page)).toHaveText("/host-diagnostics");
  await page.keyboard.type("now");
  await page.keyboard.press("Enter");
  await expect(transcript).toContainText("please /host-diagnostics now");
  await expect(field(page)).toHaveText("");
}

test("↑ in an empty composer steps back through the session's messages, and ↓ forward", async ({
  page,
}) => {
  await sendTwo(page);
  await page.keyboard.press("ArrowUp");
  await expect(history(page)).toHaveText("History 2/2");
  await expect(field(page)).toContainText("please /host-diagnostics now");
  await expect(commandChips(page)).toHaveText("/host-diagnostics");
  await page.keyboard.press("ArrowUp");
  await expect(history(page)).toHaveText("History 1/2");
  await expect(field(page)).toHaveText("first message");
  await expect(commandChips(page)).toHaveCount(0);
  /* the oldest stays put */
  await page.keyboard.press("ArrowUp");
  await expect(history(page)).toHaveText("History 1/2");
  await page.keyboard.press("ArrowDown");
  await expect(history(page)).toHaveText("History 2/2");
  await expect(commandChips(page)).toHaveText("/host-diagnostics");
  /* past the newest is an empty composer again */
  await page.keyboard.press("ArrowDown");
  await expect(history(page)).toHaveCount(0);
  await expect(field(page)).toHaveText("");
});

test("a message brought back becomes the draft once changed, and sends with its skill", async ({
  page,
}) => {
  await sendTwo(page);
  await page.keyboard.press("ArrowUp");
  await expect(history(page)).toHaveText("History 2/2");
  await page.keyboard.type(" please");
  await expect(history(page)).toHaveCount(0);
  /* ↑ now moves in the draft, not through history */
  await page.keyboard.press("ArrowUp");
  await expect(history(page)).toHaveCount(0);
  await expect(field(page)).toContainText("please /host-diagnostics now please");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("transcript-panel")).toContainText(
    /\/skill host-diagnostics\s+please \/host-diagnostics now please/,
  );
});

test("Escape puts history away and leaves the composer empty", async ({ page }) => {
  await sendTwo(page);
  await page.keyboard.press("ArrowUp");
  await page.keyboard.press("ArrowUp");
  await expect(field(page)).toHaveText("first message");
  await page.keyboard.press("Escape");
  await expect(history(page)).toHaveCount(0);
  await expect(field(page)).toHaveText("");
});

test("a composer with a draft keeps ↑ for its caret", async ({ page }) => {
  await sendTwo(page);
  await page.keyboard.type("half written");
  await page.keyboard.press("ArrowUp");
  await expect(history(page)).toHaveCount(0);
  await expect(field(page)).toHaveText("half written");
});

test("in a message of several lines, ↑ climbs its lines before the message before it", async ({
  page,
}) => {
  await sendTwo(page);
  await page.keyboard.type("line one");
  await page.keyboard.press("Shift+Enter");
  await page.keyboard.type("line two");
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("transcript-panel")).toContainText("line two");
  await expect(field(page)).toHaveText("");
  await page.keyboard.press("ArrowUp");
  await expect(history(page)).toHaveText("History 3/3");
  await expect(field(page)).toContainText("line two");
  /* the caret ends on the last line, so ↑ moves it to the first */
  await page.keyboard.press("ArrowUp");
  await expect(history(page)).toHaveText("History 3/3");
  await page.keyboard.press("ArrowUp");
  await expect(history(page)).toHaveText("History 2/3");
  await expect(commandChips(page)).toHaveText("/host-diagnostics");
});

test("a message brought back with a session chip becomes the draft once changed", async ({
  page,
}) => {
  await gotoHarness(page);
  await openChat(page);
  await field(page).click();
  await page.keyboard.type("see @intro");
  await expect(
    page.locator('[data-slot="composer-menu"]').getByRole("option").first(),
  ).toContainText("introduction-and-greetings");
  await page.keyboard.press("Enter");
  await page.keyboard.type("first");
  await page.keyboard.press("Enter");
  const transcript = page.getByTestId("transcript-panel");
  await expect(transcript).toContainText("see introduction-and-greetings first");
  await expect(field(page)).toHaveText("");
  await field(page).click();
  await page.keyboard.press("ArrowUp");
  await expect(history(page)).toHaveText("History 1/1");
  await expect(field(page).locator('[data-slot="session-chip"]')).toHaveText(
    "introduction-and-greetings",
  );
  await page.keyboard.type(" more");
  await expect(history(page)).toHaveCount(0);
  /* the edit is the draft: neither ↑ nor Escape puts the message back */
  await page.keyboard.press("ArrowUp");
  await page.keyboard.press("Escape");
  await expect(history(page)).toHaveCount(0);
  await expect(field(page)).toContainText("see introduction-and-greetings first more");
});

test("history observes the native caret before selectionchange has reached the editor", async ({
  page,
}) => {
  await sendTwo(page);
  await page.keyboard.type("line one");
  await page.keyboard.press("Shift+Enter");
  await page.keyboard.type("line two");
  await page.keyboard.press("Enter");
  await expect(field(page)).toHaveText("");
  await page.keyboard.press("ArrowUp");
  await expect(history(page)).toHaveText("History 3/3");
  await expect(field(page)).toContainText("line two");
  await field(page).evaluate((el) => {
    const text = el.querySelector("[data-lexical-text]")!.firstChild!;
    const end = text.textContent!.length;
    window.getSelection()!.setBaseAndExtent(text, end, text, end);
    el.dispatchEvent(
      new KeyboardEvent("keydown", { key: "ArrowUp", bubbles: true, cancelable: true }),
    );
  });
  await expect(history(page)).toHaveText("History 2/3");
  await expect(commandChips(page)).toHaveText("/host-diagnostics");
});
