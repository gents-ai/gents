import { composer, expect, gotoHarness, openChat, test } from "./desktopTest";

test("keeps the selected node visible and reviews a draft before changing destinations", async ({
  page,
}) => {
  await gotoHarness(page, "active-nodes");
  await openChat(page);
  await expect(page.getByLabel("Active node", { exact: true })).toContainText("Local");
  await composer(page).fill("Keep this setup seed local");
  await page.getByRole("link", { name: "Nodes", exact: true }).first().click();
  await page.getByRole("link", { name: "Amy sessions", exact: true }).click();
  const review = page.getByRole("alertdialog");
  await expect(review).toContainText("Your draft stays with Bombadil UI Agent");
  await review.getByRole("button", { name: "Keep current node" }).click();
  await expect(review).toBeHidden();
  await expect(page.getByLabel("Active node", { exact: true })).toContainText("Local");
  await page.getByRole("link", { name: "Amy sessions", exact: true }).click();
  await review.getByRole("button", { name: "Switch node" }).click();
  await expect(review).toBeHidden();
  await expect(page.getByLabel("Active node", { exact: true })).toContainText(
    "Remote · Amy on studio-1",
  );
  await expect(page.getByRole("button", { name: "Node", exact: true })).toHaveCount(0);
  await page.getByRole("button", { name: "New", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "Start a new chat with Amy" }),
  ).toBeVisible();
  await expect(composer(page)).toHaveValue("");
  await expect(page.getByTitle("Choose a node")).toHaveCount(0);
  await page.getByRole("button", { name: "Agent", exact: true }).click();
  await expect(page.getByRole("option", { name: /Remote worker/ })).toBeVisible();
  await expect(page.getByRole("option", { name: /^Default/ })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Create new agent" })).toBeDisabled();
  await page.keyboard.press("Escape");
  await page.getByLabel("Active node", { exact: true }).click();
  await expect(
    page.getByRole("status").filter({ hasText: "Remote configuration is read-only" }),
  ).toBeVisible();
  await expect(page.getByRole("textbox", { name: "Display name" })).toBeDisabled();
  await page.screenshot({ path: "/private/tmp/gents-3091-remote-settings.png" });
});
