import { expect, gotoHarness, test } from "./desktopTest";
import { chatDraftKey, chatDraftOf } from "../../src/hooks/chatDrafts";

for (const colorScheme of ["light", "dark"] as const) {
  test(`keeps a failed message compact, keyboard reachable and retryable in ${colorScheme}`, async ({
    page,
  }, info) => {
    await page.emulateMedia({ colorScheme });
    const draft = chatDraftOf(
      {
        clientDid: "did:key:desktop-harness",
        nodeDid: "did:key:z6MkBombadilAgent",
        requestId: "retained-opening",
        sessionId: "session-intro",
        agentDid: "default",
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
    const row = page.locator('[data-timeline-key="turn:retained-opening"]');
    await expect(
      row.getByText(
        "Retain this original message while preserving the newer composer draft.",
        { exact: true },
      ),
    ).toBeVisible();
    const panel = row.getByRole("status", { name: "Saved message" });
    await expect(panel).toBeVisible();
    const box = await panel.boundingBox();
    expect(box!.height).toBeLessThan(160);
    expect(box!.x).toBeGreaterThanOrEqual(0);
    expect(box!.x + box!.width).toBeLessThanOrEqual(page.viewportSize()!.width);
    const retry = panel.getByRole("button", { name: "Retry message" });
    await retry.focus();
    await expect(retry).toBeFocused();
    await page.keyboard.press("Tab");
    await expect(panel.getByRole("button", { name: "Discard message" })).toBeFocused();
    await row.screenshot({
      path: info.outputPath(`chat-delivery-${colorScheme}.png`),
    });
    await page.reload();
    await page.getByTestId("session-session-intro").click();
    await expect(panel).toBeVisible();
    await expect(panel.getByRole("button", { name: "Retry message" })).toBeEnabled();
    await page
      .getByRole("textbox", { name: "Message", exact: true })
      .fill("Keep this newer message");
    await panel.getByRole("button", { name: "Retry message" }).click();
    await expect(panel).toHaveCount(0);
    await expect(
      page.getByText(
        "Retain this original message while preserving the newer composer draft.",
        { exact: true },
      ),
    ).toBeVisible();
    await expect(
      page.getByRole("textbox", { name: "Message", exact: true }),
    ).toHaveText("Keep this newer message");
  });
}
