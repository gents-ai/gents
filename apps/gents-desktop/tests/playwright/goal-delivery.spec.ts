import { expect, gotoHarness, test } from "./desktopTest";
import { goalDraftKey, retainGoalDraft } from "../../src/hooks/goalDrafts";

for (const colorScheme of ["light", "dark"] as const) {
  test(`keeps saved Goal recovery compact and keyboard reachable in ${colorScheme}`, async ({
    page,
  }, info) => {
    await page.emulateMedia({ colorScheme });
    const draft = retainGoalDraft(
      {
        clientDid: "did:key:desktop-harness",
        nodeDid: "did:key:z6MkBombadilAgent",
        commandId: "retained-opening",
        sessionId: "session-intro",
        agentId: "default",
        sessionDocId: null,
        expected: null,
        cwd: null,
        action: {
          kind: "create",
          objective:
            "Retain this original Goal while preserving the newer composer draft.",
          tokenBudget: null,
        },
      },
      "session-intro",
    );
    await gotoHarness(page);
    await page.evaluate(({ key, value }) => localStorage.setItem(key, value), {
      key: goalDraftKey(draft),
      value: JSON.stringify(draft),
    });
    await page.reload();
    await page.getByTestId("session-session-intro").click();
    if (colorScheme === "dark")
      await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
    else await expect(page.locator("html")).not.toHaveAttribute("data-theme", "dark");
    const panel = page.getByRole("status", { name: "Saved Goal change" });
    await expect(panel).toBeVisible();
    const box = await panel.boundingBox();
    expect(box!.height).toBeLessThan(160);
    expect(box!.x).toBeGreaterThanOrEqual(0);
    expect(box!.x + box!.width).toBeLessThanOrEqual(page.viewportSize()!.width);
    const check = panel.getByRole("button", { name: "Check Goal delivery" });
    await check.focus();
    await expect(check).toBeFocused();
    await page.keyboard.press("Tab");
    await expect(
      panel.getByRole("button", { name: "Resume saved Goal change" }),
    ).toBeFocused();
    await panel.locator("summary").focus();
    await page.keyboard.press("Enter");
    await expect(
      panel.getByText(
        "Retain this original Goal while preserving the newer composer draft.",
        { exact: true },
      ),
    ).toBeVisible();
    await panel.screenshot({
      path: info.outputPath(`goal-delivery-${colorScheme}.png`),
    });
    await page.reload();
    await page.getByTestId("session-session-intro").click();
    await expect(panel).toBeVisible();
    await expect(
      panel.getByRole("button", { name: "Resume saved Goal change" }),
    ).toBeEnabled();
  });
}
