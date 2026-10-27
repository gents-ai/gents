import { expect, gotoHarness, test, type Page } from "./desktopTest";
import { chatDraftKey, retainChatDraft } from "../../src/hooks/chatDrafts";

async function openMailbox(page: Page) {
  const link = page.getByRole("link", { name: /^Mailbox(?:\s+\d+)?$/ });
  if (!(await link.first().isVisible()))
    await page.getByRole("button", { name: "Menu", exact: true }).click();
  await link.last().click();
  await expect(page.getByTestId("mailbox-screen")).toBeVisible();
}

for (const colorScheme of ["light", "dark"] as const) {
  test(`recovers a saved answer without offering it again after reload in ${colorScheme}`, async ({
    page,
  }, info) => {
    await page.emulateMedia({ colorScheme });
    await gotoHarness(page, "mailbox-question");
    const draft = retainChatDraft(
      {
        clientDid: "did:key:desktop-harness",
        nodeDid: "did:key:z6MkBombadilAgent",
        requestId: "original-answer",
        sessionId: "session-intro",
        agentId: "default",
        content: "",
        causedBySourceDocId: "mailbox-question",
        answer: { option_ids: ["local"], free_text: "Keep this local" },
      },
      "session-intro",
      false,
      { itemId: "mailbox-question", summary: "This computer\nKeep this local" },
    );
    await page.evaluate(({ key, value }) => localStorage.setItem(key, value), {
      key: chatDraftKey(draft),
      value: JSON.stringify(draft),
    });
    await page.reload();
    await openMailbox(page);
    if (colorScheme === "dark")
      await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
    else await expect(page.locator("html")).not.toHaveAttribute("data-theme", "dark");
    const panel = page.getByRole("status", { name: "Saved answer" });
    await expect(panel).toBeVisible();
    await expect(
      page.getByRole("button", { name: "This computer", exact: true }),
    ).toBeDisabled();
    const box = await panel.boundingBox();
    expect(box!.height).toBeLessThan(160);
    expect(box!.x + box!.width).toBeLessThanOrEqual(page.viewportSize()!.width);
    await panel.getByRole("button", { name: "Check answer delivery" }).focus();
    await page.keyboard.press("Tab");
    await expect(
      panel.getByRole("button", { name: "Resume saved answer" }),
    ).toBeFocused();
    await panel.getByRole("button", { name: "Check answer delivery" }).click();
    await expect(panel.getByRole("alert")).toHaveText(
      "Delivery is still unconfirmed. Check again or resume the saved answer.",
    );
    await expect(
      page.getByText("Answer sent to the agent.", { exact: true }),
    ).toHaveCount(0);
    await panel.getByRole("button", { name: "Resume saved answer" }).click();
    await expect(
      page.getByText("Answer sent to the agent.", { exact: true }),
    ).toBeVisible();
    await expect(
      panel.getByRole("button", { name: "Resume saved answer" }),
    ).toHaveCount(0);
    await expect(
      page.getByRole("button", { name: "This computer", exact: true }),
    ).toHaveCount(0);
    await panel.locator("summary").click();
    await expect(panel.getByText("Keep this local", { exact: false })).toBeVisible();
    await panel.screenshot({
      path: info.outputPath(`mailbox-answer-${colorScheme}.png`),
    });
    await page.reload();
    await openMailbox(page);
    await expect(
      page.getByText("Answer sent to the agent.", { exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "This computer", exact: true }),
    ).toHaveCount(0);
  });
}
