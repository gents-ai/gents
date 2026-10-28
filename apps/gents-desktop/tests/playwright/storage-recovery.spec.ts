import { expect, gotoHarness, test } from "./desktopTest";
import { chatDraftKey, retainChatDraft } from "../../src/hooks/chatDrafts";
import { acceptedChat } from "../chat-delivery-fixture";

for (const colorScheme of ["light", "dark"] as const) {
  test(`reviews disk-full recovery without replaying saved input in ${colorScheme}`, async ({
    page,
  }, testInfo) => {
    await page.setViewportSize({
      width: colorScheme === "dark" ? 390 : 1280,
      height: 900,
    });
    await page.emulateMedia({ colorScheme });
    const request = {
      clientDid: "did:key:desktop-harness",
      nodeDid: "did:key:z6MkBombadilAgent",
      requestId: "retained-storage-failure",
      sessionId: "session-intro",
      agentId: "default",
      cwd: null,
      content: "Retain this original message",
    };
    const draft = {
      ...retainChatDraft(request, "session-intro"),
      result: {
        ...acceptedChat(request),
        outcome: "deliveryUnknown",
        deliveryError: {
          code: "storageFull",
          message: "Storage is full",
          retryable: false,
          endpoint: null,
        },
      },
    };
    await gotoHarness(page, "storage-recovery");
    const key = chatDraftKey(draft);
    await page.evaluate(
      ({ key, draft }) => localStorage.setItem(key, JSON.stringify(draft)),
      { key, draft },
    );
    await page.reload();
    await page.getByTestId("session-session-intro").click();
    const input = page.getByRole("textbox", { name: "Message", exact: true });
    await input.fill("Keep this newer draft");
    const panel = page.getByRole("status", { name: "Saved message" });
    await panel.getByRole("button", { name: "Review runtime restart…" }).click();
    const dialog = page.getByRole("alertdialog");
    await expect(dialog).toContainText("/tmp/gents-bombadil/workspace");
    await expect(dialog).toContainText("read-only command set");
    await testInfo.attach(`review-${colorScheme}.png`, {
      body: await dialog.screenshot(),
      contentType: "image/png",
    });
    await dialog.getByRole("button", { name: "Restart runtime", exact: true }).click();
    await expect(dialog).toBeHidden();
    await expect(panel).toContainText("restarting did not resend it");
    await expect(input).toHaveText("Keep this newer draft");
    expect(
      await page.evaluate((key) => JSON.parse(localStorage.getItem(key)!), key),
    ).toEqual(draft);
    await expect(
      page.getByTestId("transcript-panel").getByText(request.content, { exact: true }),
    ).toHaveCount(0);
    const box = (await panel.boundingBox())!;
    expect(box.x).toBeGreaterThanOrEqual(0);
    expect(box.x + box.width).toBeLessThanOrEqual(page.viewportSize()!.width);
    await testInfo.attach(`recovered-${colorScheme}.png`, {
      body: await panel.screenshot(),
      contentType: "image/png",
    });
  });
}
