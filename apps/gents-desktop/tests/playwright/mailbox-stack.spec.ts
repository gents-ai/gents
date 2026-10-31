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
