import { expect, gotoHarness, test } from "./desktopTest";
import { chatDraftKey, retainChatDraft } from "../../src/hooks/chatDrafts";

for (const colorScheme of ["light", "dark"] as const) {
  test(`keeps saved message recovery compact and keyboard reachable in ${colorScheme}`, async ({
    page,
  }, info) => {
    await page.emulateMedia({ colorScheme });
    const draft = retainChatDraft(
      {
        clientDid: "did:key:desktop-harness",
        nodeDid: "did:key:z6MkBombadilAgent",
        requestId: "retained-opening",
        sessionId: "session-intro",
        agentId: "default",
        cwd: null,
        content:
          "Retain this original message while preserving the newer composer draft.",
      },
      "session-intro",
    );
    await gotoHarness(page);
    await page.evaluate(({ key, value }) => localStorage.setItem(key, value), {
      key: chatDraftKey(draft),
      value: JSON.stringify(draft),
    });
    await page.reload();
    await page.getByTestId("session-session-intro").click();
    if (colorScheme === "dark")
      await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
    else await expect(page.locator("html")).not.toHaveAttribute("data-theme", "dark");
    const panel = page.getByRole("status", { name: "Saved message" });
    await expect(panel).toBeVisible();
    const box = await panel.boundingBox();
    expect(box!.height).toBeLessThan(160);
    expect(box!.x).toBeGreaterThanOrEqual(0);
    expect(box!.x + box!.width).toBeLessThanOrEqual(page.viewportSize()!.width);
    const check = panel.getByRole("button", { name: "Check message delivery" });
    await check.focus();
    await expect(check).toBeFocused();
    await page.keyboard.press("Tab");
    await expect(
      panel.getByRole("button", { name: "Resume saved message" }),
    ).toBeFocused();
    await panel.locator("summary").focus();
    await page.keyboard.press("Enter");
    await expect(
      panel.getByText(
        "Retain this original message while preserving the newer composer draft.",
        { exact: true },
      ),
    ).toBeVisible();
    await panel.screenshot({
      path: info.outputPath(`chat-delivery-${colorScheme}.png`),
    });
    await page.reload();
    await page.getByTestId("session-session-intro").click();
    await expect(panel).toBeVisible();
    await expect(
      panel.getByRole("button", { name: "Resume saved message" }),
    ).toBeEnabled();
    await page
      .getByRole("textbox", { name: "Message", exact: true })
      .fill("Keep this newer message");
    await panel.getByRole("button", { name: "Check message delivery" }).click();
    await expect(panel.getByRole("alert")).toHaveText(
      "Delivery is still unconfirmed. Check again or resume the saved message.",
    );
    await panel.getByRole("button", { name: "Resume saved message" }).click();
    await expect(panel).toHaveCount(0);
    await expect(
      page.getByText(
        "Retain this original message while preserving the newer composer draft.",
        { exact: true },
      ),
    ).toBeVisible();
    await expect(
      page.getByRole("textbox", { name: "Message", exact: true }),
    ).toHaveValue("Keep this newer message");
  });
}
